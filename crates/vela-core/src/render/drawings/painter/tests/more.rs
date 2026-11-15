//! The magnifier, the placement helpers (ghost, marquee, snap ring, hint) and the tools
//! whose output the family tests do not cover.

use std::f64::consts::PI;

use super::*;
use crate::drawings::Bounds;
use crate::drawings::types::measure::magnifier_timeframe_label;
use crate::i18n::source_text;

fn sample_px(x: f64, y: f64) -> DrawingPoint {
    point(x * 60_000.0, 200.0 - 0.25 * y)
}

fn magnifier() -> Box<dyn Drawing> {
    make(
        "magnifier",
        "mg",
        vec![sample_px(100.0, 300.0), sample_px(400.0, 500.0)],
        None,
        None,
    )
}

fn gateway(state: DrawingSeriesState) -> GatewayProj {
    GatewayProj {
        inner: LinearProjector::sample(),
        state,
    }
}

fn bar(time_px: f64, o: f64, h: f64, l: f64, c: f64) -> Ohlcv {
    Ohlcv::new((time_px * 60_000.0) as i64, o, h, l, c, None)
}

fn ready(bars: Vec<Ohlcv>, bar_ms: f64) -> DrawingSeriesState {
    DrawingSeriesState::Ready {
        bars,
        timeframe: "1".to_string(),
        bar_ms,
    }
}

/// The ops of the opaque backdrop every painted magnifier starts with (inside the pane clip).
fn backdrop() -> Vec<Value> {
    vec![
        json!(["save"]),
        json!(["globalAlpha", 1.0]),
        json!(["fillStyle", "#101010"]),
        json!(["fillRect", 100.0, 300.0, 300.0, 200.0]),
        json!(["restore"]),
    ]
}

#[test]
fn a_magnifier_paints_candles_a_border_and_a_chip() {
    let d = magnifier();
    // A 10-minute bar at x = 200 (10 px wide): shifted half a chart bar (0.5 px) left.
    let proj = gateway(ready(
        vec![bar(200.0, 120.0, 135.0, 115.0, 130.0)],
        600_000.0,
    ));
    let mut painter = DrawingPainter::new();
    let canvas = RecordingCanvas::new();
    painter.paint_all(
        &canvas,
        &[d.as_ref()],
        &proj,
        &theme(),
        &PaintTargets::default(),
    );

    let (cx0, cx1) = (200.0 - 0.5, 210.0 - 0.5);
    let cx = (cx0 + cx1) / 2.0;
    let bw = (10.0f64 * 0.7).max(1.0);
    let label = source_text(&magnifier_timeframe_label("1"));
    let tw = label.chars().count() as f64 * 10.0 * 0.6;
    let (chip_w, chip_h, chip_y) = (tw + 12.0 + 11.0, 17.0, 500.0 + 4.0);
    let mut e = vec![
        json!(["save"]),
        json!(["beginPath"]),
        json!(["rect", 0.0, 0.0, 1000.0, 600.0]),
        json!(["clip"]),
    ];
    e.extend(backdrop());
    e.extend([
        json!(["save"]),
        json!(["beginPath"]),
        json!(["rect", 100.0, 300.0, 300.0, 200.0]),
        json!(["clip"]),
        json!(["setLineDash", []]),
        json!(["lineWidth", 1.0]),
        json!(["strokeStyle", "#089981"]),
        json!(["beginPath"]),
        json!(["moveTo", cx, 260.0]),
        json!(["lineTo", cx, 340.0]),
        json!(["stroke"]),
        json!(["fillStyle", "#089981"]),
        json!(["fillRect", cx - bw / 2.0, 280.0, bw, 40.0]),
        json!(["restore"]),
        // The border: no color of its own, so the theme's contrast ink.
        json!(["save"]),
        json!(["strokeStyle", "#ffffff"]),
        json!(["lineWidth", 1.0]),
        json!(["setLineDash", []]),
        json!(["strokeRect", 100.0, 300.0, 300.0, 200.0]),
        json!(["restore"]),
        // The chip rides the bottom-left corner, below the rectangle.
        json!(["save"]),
        json!(["font", "10px sans-serif"]),
        json!(["beginPath"]),
        json!(["moveTo", 103.0, chip_y]),
        json!([
            "arcTo",
            100.0 + chip_w,
            chip_y,
            100.0 + chip_w,
            chip_y + chip_h,
            3.0
        ]),
        json!([
            "arcTo",
            100.0 + chip_w,
            chip_y + chip_h,
            100.0,
            chip_y + chip_h,
            3.0
        ]),
        json!(["arcTo", 100.0, chip_y + chip_h, 100.0, chip_y, 3.0]),
        json!(["arcTo", 100.0, chip_y, 100.0 + chip_w, chip_y, 3.0]),
        json!(["closePath"]),
        json!(["fillStyle", "#101010"]),
        json!(["fill"]),
        json!(["strokeStyle", "rgba(255,255,255,0.28)"]),
        json!(["lineWidth", 1.0]),
        json!(["setLineDash", []]),
        json!(["stroke"]),
        json!(["fillStyle", "#ffffff"]),
        json!(["textAlign", "left"]),
        json!(["textBaseline", "middle"]),
        json!(["fillText", label, 106.0, chip_y + chip_h / 2.0 + 0.5]),
        json!(["strokeStyle", "rgba(255,255,255,0.7)"]),
        json!(["lineWidth", 1.2]),
        json!(["beginPath"]),
        json!(["moveTo", 106.0 + tw + 5.0, chip_y + 8.5 - 1.5]),
        json!(["lineTo", 106.0 + tw + 5.0 + 2.5, chip_y + 8.5 + 1.5]),
        json!(["lineTo", 106.0 + tw + 5.0 + 5.0, chip_y + 8.5 - 1.5]),
        json!(["stroke"]),
        json!(["restore"]),
        json!(["restore"]),
    ]);
    assert_eq!(canvas.to_json(), Value::Array(e));
    assert_eq!(
        painter.chip_rect("mg"),
        Some(Bounds {
            x: 100.0,
            y: chip_y,
            w: chip_w,
            h: chip_h
        })
    );
}

#[test]
fn a_magnifier_bar_narrower_than_three_px_is_just_its_wick() {
    let d = magnifier();
    let proj = gateway(ready(
        vec![bar(200.0, 120.0, 135.0, 115.0, 110.0)],
        120_000.0,
    ));
    let canvas = paint_one_drawing(d.as_ref(), &proj);
    // Down candle (close < open) in the down color; a 2 px cell: no body.
    assert!(canvas.ops().contains(&json!(["strokeStyle", "#f23645"])));
    assert!(canvas.calls("fillRect").len() == 1); // the backdrop only
}

#[test]
fn an_ohlc_bar_inset_draws_open_and_close_ticks() {
    let d = magnifier();
    let proj = gateway(ready(
        vec![bar(200.0, 120.0, 135.0, 115.0, 130.0)],
        600_000.0,
    ));
    let mut painter = DrawingPainter::new();
    painter.series_look.style = "bars".to_string();
    let canvas = RecordingCanvas::new();
    painter.paint_all(
        &canvas,
        &[d.as_ref()],
        &proj,
        &theme(),
        &PaintTargets::default(),
    );
    let cx = ((200.0 - 0.5) + (210.0 - 0.5)) / 2.0;
    let tick = (10.0f64 * 0.35).max(1.0);
    let ops = canvas.ops();
    for want in [
        json!(["moveTo", cx - tick, 320.0]),
        json!(["lineTo", cx, 320.0]),
        json!(["moveTo", cx, 280.0]),
        json!(["lineTo", cx + tick, 280.0]),
    ] {
        assert!(ops.contains(&want), "{want}");
    }
    assert_eq!(canvas.calls("fillRect").len(), 1); // no body fill
}

#[test]
fn a_line_and_an_area_inset_follow_the_closes() {
    let d = magnifier();
    let bars = vec![
        bar(150.0, 120.0, 135.0, 115.0, 125.0),
        bar(200.0, 125.0, 135.0, 115.0, 130.0),
        bar(250.0, 130.0, 140.0, 120.0, 100.0),
    ];
    let proj = gateway(ready(bars, 300_000.0));
    for (style, area) in [("line", false), ("area", true)] {
        let mut painter = DrawingPainter::new();
        painter.series_look.style = style.to_string();
        let canvas = RecordingCanvas::new();
        painter.paint_all(
            &canvas,
            &[d.as_ref()],
            &proj,
            &theme(),
            &PaintTargets::default(),
        );
        let ops = canvas.ops();
        // Points sit at cell centers, half a chart bar (0.5 px) left; y = (200 - close) * 4.
        let pts = [
            (150.0 + 2.5 - 0.5, 300.0),
            (200.0 + 2.5 - 0.5, 280.0),
            (250.0 + 2.5 - 0.5, 400.0),
        ];
        let tail = ops
            .iter()
            .position(|o| *o == json!(["lineWidth", 1.5]))
            .unwrap();
        assert_eq!(
            ops[tail..tail + 7].to_vec(),
            vec![
                json!(["lineWidth", 1.5]),
                json!(["strokeStyle", "#089981"]),
                json!(["beginPath"]),
                json!(["moveTo", pts[0].0, pts[0].1]),
                json!(["lineTo", pts[1].0, pts[1].1]),
                json!(["lineTo", pts[2].0, pts[2].1]),
                json!(["stroke"]),
            ]
        );
        let fills = ops
            .iter()
            .filter(|o| **o == json!(["fillStyle", "rgba(8,153,129,0.15)"]))
            .count();
        assert_eq!(fills, usize::from(area), "{style}");
    }
}

#[test]
fn a_magnifier_color_of_its_own_wins_over_the_series_look() {
    let d = make(
        "magnifier",
        "mg",
        vec![sample_px(100.0, 300.0), sample_px(400.0, 500.0)],
        Some(json!({ "upColor": "#112233", "downColor": "#445566" })),
        None,
    );
    let proj = gateway(ready(
        vec![bar(200.0, 120.0, 135.0, 115.0, 130.0)],
        600_000.0,
    ));
    let canvas = paint_one_drawing(d.as_ref(), &proj);
    assert!(canvas.ops().contains(&json!(["strokeStyle", "#112233"])));
}

#[test]
fn a_magnifier_without_bars_says_why() {
    let d = magnifier();
    let cases = [
        (
            DrawingSeriesState::Loading {
                timeframe: "1".into(),
                bar_ms: 60_000.0,
                bars: None,
            },
            "Loading\u{2026}",
        ),
        (ready(vec![], 60_000.0), "No lower-timeframe data"),
        (
            DrawingSeriesState::Unavailable {
                reason: SeriesUnavailable::TooWide,
            },
            "Area too wide for this timeframe",
        ),
        (
            DrawingSeriesState::Unavailable {
                reason: SeriesUnavailable::NoneLower,
            },
            "No lower timeframe available",
        ),
        (
            DrawingSeriesState::Unavailable {
                reason: SeriesUnavailable::NotLower,
            },
            "Pick a timeframe lower than the chart's",
        ),
        (
            DrawingSeriesState::Unavailable {
                reason: SeriesUnavailable::NoSource,
            },
            "No data source",
        ),
    ];
    for (state, notice) in cases {
        let canvas = paint_one_drawing(d.as_ref(), &gateway(state));
        let texts = canvas.texts();
        assert!(texts.contains(&notice.to_string()), "{notice}: {texts:?}");
        assert_sane(&canvas, notice);
    }
    // A partial `Loading` still paints what it has instead of the notice.
    let partial = DrawingSeriesState::Loading {
        timeframe: "1".into(),
        bar_ms: 600_000.0,
        bars: Some(vec![bar(200.0, 120.0, 135.0, 115.0, 130.0)]),
    };
    let canvas = paint_one_drawing(d.as_ref(), &gateway(partial));
    assert!(!canvas.texts().contains(&"Loading\u{2026}".to_string()));
    assert!(canvas.ops().contains(&json!(["strokeStyle", "#089981"])));
}

#[test]
fn a_small_magnifier_skips_the_notice_and_the_chip() {
    let d = make(
        "magnifier",
        "small",
        vec![sample_px(100.0, 300.0), sample_px(130.0, 310.0)],
        None,
        None,
    );
    let proj = gateway(ready(vec![], 60_000.0));
    let mut painter = DrawingPainter::new();
    let canvas = RecordingCanvas::new();
    painter.paint_all(
        &canvas,
        &[d.as_ref()],
        &proj,
        &theme(),
        &PaintTargets::default(),
    );
    assert!(canvas.texts().is_empty());
    assert_eq!(painter.chip_rect("small"), None);
}

#[test]
fn a_magnifier_without_a_series_gateway_paints_backdrop_and_border_only() {
    let d = magnifier();
    let canvas = paint_one_drawing(d.as_ref(), &LinearProjector::sample());
    assert!(canvas.texts().iter().all(|t| t != "No data source"));
    assert!(
        canvas
            .ops()
            .contains(&json!(["strokeRect", 100.0, 300.0, 300.0, 200.0]))
    );
}

#[test]
fn the_magnifier_ghost_is_a_dashed_outline_only() {
    let d = magnifier();
    let canvas = RecordingCanvas::new();
    DrawingPainter::new().paint_ghost(&canvas, d.as_ref(), &gateway(ready(vec![], 1.0)), &theme());
    assert_eq!(
        canvas.to_json(),
        json!([
            ["globalAlpha", 0.7],
            ["save"],
            ["strokeStyle", "#ffffff"],
            ["lineWidth", 1.0],
            ["setLineDash", [4.0, 4.0]],
            ["strokeRect", 100.0, 300.0, 300.0, 200.0],
            ["restore"],
            ["globalAlpha", 1.0],
        ])
    );
}

#[test]
fn a_ghost_paints_the_drawing_at_reduced_opacity() {
    let d = make(
        "trendline",
        "g",
        vec![point(10.0, 20.0), point(110.0, 60.0)],
        None,
        None,
    );
    let canvas = RecordingCanvas::new();
    DrawingPainter::new().paint_ghost(
        &canvas,
        d.as_ref(),
        &FnProj::new(100.0, 300.0, 100.0),
        &theme(),
    );
    let ops = canvas.ops();
    assert_eq!(ops[0], json!(["globalAlpha", 0.7]));
    assert_eq!(ops[ops.len() - 1], json!(["globalAlpha", 1.0]));
    assert_eq!(ops.len(), 10); // alpha, style (3), path (3), stroke, dash, alpha
}

#[test]
fn a_time_span_ghost_shows_two_guides_a_connecting_line_and_handles() {
    for key in ["regressionchannel", "fixedrangevp"] {
        let d = make(
            key,
            "g",
            vec![sample_px(100.0, 300.0), sample_px(300.0, 200.0)],
            None,
            None,
        );
        let canvas = RecordingCanvas::new();
        DrawingPainter::new().paint_ghost(
            &canvas,
            d.as_ref(),
            &LinearProjector::sample(),
            &theme(),
        );
        // Neutral gray guides (the regression midline's own color is the same gray).
        let guide = |x: f64| {
            vec![
                json!(["save"]),
                json!(["strokeStyle", "#787b86"]),
                json!(["globalAlpha", 0.6 * 0.7]),
                json!(["lineWidth", 1.0]),
                json!(["setLineDash", [4.0, 4.0]]),
                json!(["beginPath"]),
                json!(["moveTo", x, 0.0]),
                json!(["lineTo", x, 600.0]),
                json!(["stroke"]),
                json!(["restore"]),
            ]
        };
        let mut e = vec![json!(["globalAlpha", 0.7])];
        e.extend(guide(100.0));
        e.extend(guide(300.0));
        e.extend([
            json!(["save"]),
            json!(["strokeStyle", "#787b86"]),
            json!(["lineWidth", 1.0]),
            json!(["setLineDash", []]),
            json!(["beginPath"]),
            json!(["moveTo", 100.0, 300.0]),
            json!(["lineTo", 300.0, 200.0]),
            json!(["stroke"]),
            json!(["restore"]),
            json!(["setLineDash", []]),
            json!(["lineWidth", 1.5]),
        ]);
        for (x, y) in [(100.0, 300.0), (300.0, 200.0)] {
            e.extend([
                json!(["beginPath"]),
                json!(["arc", x, y, 4.5, 0.0, PI * 2.0, false]),
                json!(["fillStyle", "rgba(120,123,134,0.55)"]),
                json!(["fill"]),
                json!(["strokeStyle", "#38c0fd"]),
                json!(["stroke"]),
            ]);
        }
        e.push(json!(["globalAlpha", 1.0]));
        assert_eq!(canvas.to_json(), Value::Array(e), "{key}");
    }
}

#[test]
fn a_time_span_ghost_with_one_anchor_is_one_guide_and_a_handle() {
    let d = make(
        "fixedrangevp",
        "g",
        vec![sample_px(100.0, 300.0)],
        None,
        None,
    );
    let canvas = RecordingCanvas::new();
    DrawingPainter::new().paint_ghost(&canvas, d.as_ref(), &LinearProjector::sample(), &theme());
    assert_eq!(canvas.calls("save").len(), 1);
    assert_eq!(canvas.calls("arc").len(), 1);
    let none = make("fixedrangevp", "g", vec![], None, None);
    let empty = RecordingCanvas::new();
    DrawingPainter::new().paint_ghost(&empty, none.as_ref(), &LinearProjector::sample(), &theme());
    assert_eq!(
        empty.to_json(),
        json!([["globalAlpha", 0.7], ["globalAlpha", 1.0]])
    );
}

#[test]
fn the_marquee_the_snap_ring_and_the_placement_hint() {
    let canvas = RecordingCanvas::new();
    let painter = DrawingPainter::new();
    painter.paint_marquee(&canvas, 10.0, 20.0, 30.0, 40.0);
    assert_eq!(
        canvas.to_json(),
        json!([
            ["save"],
            ["fillStyle", "rgba(56,192,253,0.08)"],
            ["fillRect", 10.0, 20.0, 30.0, 40.0],
            ["strokeStyle", "#38c0fd"],
            ["lineWidth", 1.0],
            ["setLineDash", [4.0, 3.0]],
            ["strokeRect", 10.5, 20.5, 30.0, 40.0],
            ["restore"],
        ])
    );

    let ring = RecordingCanvas::new();
    painter.paint_snap_ring(&ring, 50.0, 60.0, &theme());
    assert_eq!(
        ring.to_json(),
        json!([
            ["setLineDash", []],
            ["globalAlpha", 0.9],
            ["beginPath"],
            ["arc", 50.0, 60.0, 5.5, 0.0, PI * 2.0, false],
            ["strokeStyle", "#ffffff"],
            ["lineWidth", 1.5],
            ["stroke"],
            ["globalAlpha", 1.0],
        ])
    );

    let hint = RecordingCanvas::new();
    painter.paint_placement_hint(&hint, "Drag an area", &theme(), 400.0, 300.0);
    let tw = 12.0 * 11.0 * 0.6;
    let (pill_w, pill_h) = (tw + 24.0, 24.0);
    let (x, y) = ((400.0 - pill_w) / 2.0, 300.0 - pill_h - 14.0);
    let ops = hint.ops();
    assert_eq!(ops[0], json!(["save"]));
    assert_eq!(ops[1], json!(["font", "11px sans-serif"]));
    assert_eq!(ops[3], json!(["moveTo", x + 12.0, y]));
    assert_eq!(
        ops[ops.len() - 2],
        json!(["fillText", "Drag an area", 200.0, y + 12.0 + 0.5])
    );
    assert_eq!(ops[ops.len() - 1], json!(["restore"]));
}

#[test]
fn a_highlighter_is_one_round_capped_swath() {
    let d = make(
        "highlighter",
        "h",
        vec![point(10.0, 20.0), point(60.0, 50.0), point(110.0, 30.0)],
        None,
        None,
    );
    let style = d.base().style.clone();
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 300.0, 100.0));
    assert_eq!(
        canvas.to_json(),
        json!([
            ["save"],
            ["strokeStyle", style.line_color],
            ["lineWidth", style.line_width],
            ["lineCap", "round"],
            ["lineJoin", "round"],
            ["setLineDash", []],
            ["beginPath"],
            ["moveTo", 10.0, 80.0],
            ["lineTo", 60.0, 50.0],
            ["lineTo", 110.0, 70.0],
            ["stroke"],
            ["restore"],
        ])
    );
}

#[test]
fn a_path_ends_in_an_arrowhead_along_its_last_segment() {
    let d = make(
        "path",
        "p",
        vec![point(10.0, 20.0), point(60.0, 50.0), point(110.0, 30.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 300.0, 100.0));
    let ops = canvas.ops();
    // Two segment strokes, then the head at (110, 70) pointing along (60,50) to (110,70).
    assert_eq!(canvas.calls("stroke").len(), 2);
    let head = ops
        .iter()
        .position(|o| *o == json!(["moveTo", 110.0, 70.0]))
        .unwrap();
    assert_eq!(ops[head - 4], json!(["save"]));
    assert_eq!(ops[head + 3], json!(["closePath"]));
    assert_eq!(ops[head + 4], json!(["fill"]));
}

#[test]
fn an_info_line_reads_change_percent_and_bars_in_a_badge() {
    let d = make(
        "infoline",
        "i",
        vec![sample_px(100.0, 300.0), sample_px(300.0, 200.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &LinearProjector::sample());
    // 125 to 150: +25.00 (+20.00%), 200 bars on the sample projector.
    let text = "+25.00 (+20.00%)  200 bars";
    assert!(canvas.texts().contains(&text.to_string()));
    let w = text.chars().count() as f64 * 11.0 * 0.6 + 12.0;
    let (mx, my) = (200.0, 250.0);
    let (bx, by) = (mx - 2.0, my - 18.0 - 4.0);
    let ops = canvas.ops();
    let at = ops
        .iter()
        .position(|o| *o == json!(["font", "11px monospace"]))
        .unwrap();
    assert_eq!(ops[at + 1], json!(["save"]));
    assert_eq!(ops[at + 2], json!(["globalAlpha", 0.9]));
    assert_eq!(ops[at + 4], json!(["moveTo", bx + 3.0, by]));
    assert_eq!(ops[at + 3], json!(["beginPath"]));
    let rect_end = at + 3 + 6;
    assert_eq!(ops[rect_end], json!(["closePath"]));
    assert_eq!(ops[rect_end + 1], json!(["fillStyle", "#1e293b"]));
    assert_eq!(ops[rect_end + 3], json!(["strokeStyle", "#475569"]));
    assert_eq!(
        canvas.calls("fillText").last().unwrap(),
        &json!(["fillText", text, bx + 6.0, by + 9.0])
    );
    assert!(w > 12.0);
}

#[test]
fn a_price_label_tag_shows_the_formatted_price() {
    let d = make("pricelabel", "l", vec![point(50.0, 123.456)], None, None);
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(200.0, 300.0, 200.0));
    // >= 100: no decimals, with grouping.
    assert_eq!(canvas.texts(), ["123"]);
    let big = make(
        "pricenote",
        "n",
        vec![point(50.0, 12345.678), point(90.0, 12345.678)],
        None,
        None,
    );
    let canvas = paint_one_drawing(big.as_ref(), &FnProj::new(20000.0, 300.0, 200.0));
    assert_eq!(canvas.texts(), ["12,346"]);
}

#[test]
fn a_note_and_a_signpost_fall_back_to_their_placeholder_text() {
    let mut empty = text_block("");
    empty.size = BoxTextSize::Normal;
    let note = make(
        "note",
        "n",
        vec![point(50.0, 50.0)],
        None,
        Some(empty.clone()),
    );
    assert_eq!(
        paint_one_drawing(note.as_ref(), &FnProj::new(100.0, 300.0, 100.0)).texts(),
        ["Note"]
    );
    let sign = make(
        "signpost",
        "s",
        vec![point(50.0, 20.0), point(90.0, 60.0)],
        None,
        Some(empty),
    );
    assert_eq!(
        paint_one_drawing(sign.as_ref(), &FnProj::new(100.0, 300.0, 100.0)).texts(),
        ["Signpost"]
    );
}

#[test]
fn a_glyph_stamp_is_centered_text_at_its_size() {
    let d = make(
        "iconstamp",
        "s",
        vec![point(50.0, 50.0)],
        Some(json!({ "size": "large" })),
        None,
    );
    let style = d.base().style.clone();
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 300.0, 100.0));
    assert_eq!(
        canvas.to_json(),
        json!([
            ["save"],
            ["setLineDash", []],
            ["font", "32px sans-serif"],
            ["fillStyle", style.line_color],
            ["textAlign", "center"],
            ["textBaseline", "middle"],
            ["fillText", "\u{2605}", 50.0, 50.0],
            ["textAlign", "left"],
            ["textBaseline", "alphabetic"],
            ["restore"],
        ])
    );
}

#[test]
fn a_regression_channel_shows_its_bands_lines_and_r_squared_badge() {
    let d = make(
        "regressionchannel",
        "r",
        vec![sample_px(100.0, 300.0), sample_px(300.0, 300.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &bars_projector());
    let texts = canvas.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].starts_with("R\u{b2} ") && texts[0].ends_with('%'),
        "{texts:?}"
    );
    // Two quads (save, fillStyle, path of 4 + close, fill, restore), three lines, the badge.
    assert_eq!(canvas.calls("closePath").len(), 3);
    assert_eq!(canvas.calls("fill").len(), 3);
    assert_eq!(canvas.calls("stroke").len(), 3);
    assert_sane(&canvas, "regression");
}

#[test]
fn a_volume_profile_draws_rows_levels_and_an_empty_one_nothing() {
    let d = make(
        "fixedrangevp",
        "v",
        vec![sample_px(100.0, 300.0), sample_px(300.0, 300.0)],
        None,
        None,
    );
    let with_bars = paint_one_drawing(d.as_ref(), &bars_projector());
    assert!(!with_bars.calls("fillRect").is_empty());
    assert_sane(&with_bars, "frvp");
    let without = paint_one_drawing(d.as_ref(), &LinearProjector::sample());
    assert!(without.calls("fillRect").is_empty());
}

#[test]
fn an_anchored_vwap_draws_three_polylines_and_a_band() {
    let d = make(
        "anchoredvwap",
        "w",
        vec![sample_px(100.0, 300.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &bars_projector());
    assert_sane(&canvas, "vwap");
    // The midline always paints; the band edges are transparent by default and skip.
    assert!(!canvas.calls("stroke").is_empty());
}

#[test]
fn liquidity_draws_its_line_and_marks_the_sweep() {
    let d = make(
        "liquidity",
        "q",
        vec![point(35.0 * 300_000.0, 120.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &bars_projector());
    assert_sane(&canvas, "liquidity");
    // The line, then (when swept) a 1.75 px cross in the same color.
    let widths: Vec<f64> = canvas
        .calls("lineWidth")
        .iter()
        .map(|o| o[1].as_f64().unwrap())
        .collect();
    assert_eq!(widths.first(), Some(&1.0));
}

#[test]
fn the_wavefront_and_tessellation_tools_clip_and_stroke() {
    let sonic = make(
        "sonic",
        "s",
        vec![sample_px(100.0, 300.0), sample_px(200.0, 300.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(sonic.as_ref(), &LinearProjector::sample());
    assert_sane(&canvas, "sonic");
    assert!(!canvas.texts().is_empty()); // ratio labels
    let dedekind = make(
        "dedekind",
        "d",
        vec![sample_px(100.0, 200.0), sample_px(400.0, 500.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(dedekind.as_ref(), &LinearProjector::sample());
    assert_sane(&canvas, "dedekind");
    // The light border at 0.45 alpha, then a clipped tessellation.
    assert!(canvas.ops().contains(&json!(["globalAlpha", 0.45])));
    assert!(canvas.calls("clip").len() >= 2);
}

/// A translator that brackets every message, to prove the painter reads the chart's one.
struct Bracketing;

impl Translator for Bracketing {
    fn locale(&self) -> &str {
        crate::i18n::english_translator().locale()
    }
    fn direction(&self) -> crate::i18n::TextDirection {
        crate::i18n::english_translator().direction()
    }
    fn numbering_system(&self) -> &str {
        crate::i18n::english_translator().numbering_system()
    }
    fn hour_cycle(&self) -> crate::format::types::HourCycleSetting {
        crate::i18n::english_translator().hour_cycle()
    }
    fn week_start(&self) -> u8 {
        crate::i18n::english_translator().week_start()
    }
    fn t_with(&self, message: &MessageLike, args: &[(&str, MessageArg)]) -> String {
        format!(
            "[{}]",
            crate::i18n::english_translator().t_with(message, args)
        )
    }
    fn format(&self) -> &crate::format::formatters::ResolvedFormatters {
        crate::i18n::english_translator().format()
    }
}

#[test]
fn the_painter_reads_the_charts_shared_translator() {
    let mut painter = DrawingPainter::new();
    let m = crate::i18n::msg("x.id", "Label");
    assert_eq!(painter.t(&m), "Label");
    // The shell's translator is an `Rc`: the painter takes it as it is.
    painter.i18n = Some(Rc::new(Bracketing) as Rc<dyn Translator>);
    assert_eq!(painter.t(&m), "[Label]");
}
