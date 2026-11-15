//! Dumps what the indicator-drawing painters draw, as recorded canvas call
//! streams: generated sets of lines, boxes, labels, polylines and linefills
//! through `DrawingSceneRenderer`, and generated tables through `paint_table`.
//! Each scenario carries its input, so the parity script can replay it on the
//! original TypeScript painters (see the README).
//!
//! Usage: `cargo run --release --example dump_scene > scene.json`

use serde_json::{Value, json};
use vela_core::model::LineStyle;
use vela_core::model::drawings::{
    BoxFontFamily, BoxHAlign, BoxTextSize, BoxVAlign, DrawingBox, DrawingExtend, DrawingLabel,
    DrawingLine, DrawingLinefill, DrawingPolyline, DrawingTable, DrawingXLoc, LabelStyle,
    LabelYLoc, PolylinePoint, TableCell, TableMerge, TablePosition, TableTextSize,
};
use vela_core::render::canvas::RecordingCanvas;
use vela_core::render::shared::drawing_scene_renderer::{
    BarHighLow, DrawingSceneRenderer, DrawingSet, LabelTipRegion, LayerDeps,
};
use vela_core::render::shared::table_canvas_renderer::{TablePaintArgs, paint_table};

const PANE: &str = "price";
const W: f64 = 1000.0;
const H: f64 = 600.0;
const BARS: usize = 200;
const MINUTE: f64 = 60_000.0;

/// Lehmer generator: every product stays under 2^53, so the numbers are exact in JavaScript too.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = (self.0 * 48271) % 2_147_483_647;
        self.0
    }
    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len() as u64)]
    }
    /// A price on a quarter grid, inside and a little outside the view.
    fn price(&mut self) -> f64 {
        self.below(720) as f64 / 4.0 - 15.0
    }
    /// A bar index, inside and a little outside the view.
    fn index(&mut self) -> f64 {
        self.below(230) as f64 - 10.0
    }
}

const COLORS: [Option<&str>; 6] = [
    None,
    Some("#2962ff"),
    Some("#ff000080"),
    Some("rgba(10, 200, 30, 0.4)"),
    Some("#fff"),
    Some("#000000"),
];
const XLOCS: [(DrawingXLoc, &str); 2] = [
    (DrawingXLoc::BarIndex, "bar_index"),
    (DrawingXLoc::BarTime, "bar_time"),
];
const EXTENDS: [(DrawingExtend, &str); 4] = [
    (DrawingExtend::None, "none"),
    (DrawingExtend::Left, "left"),
    (DrawingExtend::Right, "right"),
    (DrawingExtend::Both, "both"),
];
const STYLES: [(LineStyle, &str); 3] = [
    (LineStyle::Solid, "solid"),
    (LineStyle::Dashed, "dashed"),
    (LineStyle::Dotted, "dotted"),
];
const SIZES: [(BoxTextSize, &str); 6] = [
    (BoxTextSize::Auto, "auto"),
    (BoxTextSize::Tiny, "tiny"),
    (BoxTextSize::Small, "small"),
    (BoxTextSize::Normal, "normal"),
    (BoxTextSize::Large, "large"),
    (BoxTextSize::Huge, "huge"),
];
const H_ALIGNS: [(BoxHAlign, &str); 3] = [
    (BoxHAlign::Left, "left"),
    (BoxHAlign::Center, "center"),
    (BoxHAlign::Right, "right"),
];
const V_ALIGNS: [(BoxVAlign, &str); 3] = [
    (BoxVAlign::Top, "top"),
    (BoxVAlign::Center, "center"),
    (BoxVAlign::Bottom, "bottom"),
];
const FONTS: [(BoxFontFamily, &str); 2] = [
    (BoxFontFamily::Default, "default"),
    (BoxFontFamily::Monospace, "monospace"),
];
const LABEL_STYLES: [(LabelStyle, &str); 21] = [
    (LabelStyle::LabelUp, "label_up"),
    (LabelStyle::LabelDown, "label_down"),
    (LabelStyle::LabelLeft, "label_left"),
    (LabelStyle::LabelRight, "label_right"),
    (LabelStyle::LabelCenter, "label_center"),
    (LabelStyle::LabelLowerLeft, "label_lower_left"),
    (LabelStyle::LabelLowerRight, "label_lower_right"),
    (LabelStyle::LabelUpperLeft, "label_upper_left"),
    (LabelStyle::LabelUpperRight, "label_upper_right"),
    (LabelStyle::Circle, "circle"),
    (LabelStyle::Square, "square"),
    (LabelStyle::Diamond, "diamond"),
    (LabelStyle::Flag, "flag"),
    (LabelStyle::ArrowUp, "arrowup"),
    (LabelStyle::ArrowDown, "arrowdown"),
    (LabelStyle::TriangleUp, "triangleup"),
    (LabelStyle::TriangleDown, "triangledown"),
    (LabelStyle::Cross, "cross"),
    (LabelStyle::XCross, "xcross"),
    (LabelStyle::TextOutline, "text_outline"),
    (LabelStyle::None, "none"),
];
const YLOCS: [(LabelYLoc, &str); 5] = [
    (LabelYLoc::Price, "price"),
    (LabelYLoc::AboveBar, "abovebar"),
    (LabelYLoc::BelowBar, "belowbar"),
    (LabelYLoc::Top, "top"),
    (LabelYLoc::Bottom, "bottom"),
];
const POSITIONS: [(TablePosition, &str); 9] = [
    (TablePosition::TopLeft, "top_left"),
    (TablePosition::TopCenter, "top_center"),
    (TablePosition::TopRight, "top_right"),
    (TablePosition::MiddleLeft, "middle_left"),
    (TablePosition::MiddleCenter, "middle_center"),
    (TablePosition::MiddleRight, "middle_right"),
    (TablePosition::BottomLeft, "bottom_left"),
    (TablePosition::BottomCenter, "bottom_center"),
    (TablePosition::BottomRight, "bottom_right"),
];
const TEXTS: [Option<&str>; 7] = [
    None,
    Some(""),
    Some("Hi"),
    Some("Order block"),
    Some("Two\nlines"),
    Some("A fairly long caption that will need wrapping inside a narrow box"),
    Some("-12.5%"),
];

fn color(rng: &mut Rng) -> Option<String> {
    rng.pick(&COLORS).map(str::to_string)
}

/// An x for `xloc`: a bar index, or that bar's time.
fn x_for(xloc: DrawingXLoc, index: f64) -> f64 {
    match xloc {
        DrawingXLoc::BarIndex => index,
        DrawingXLoc::BarTime => index * MINUTE,
    }
}

fn line(rng: &mut Rng, id: String) -> (DrawingLine, Value) {
    let (xloc, xloc_s) = rng.pick(&XLOCS);
    let (extend, extend_s) = rng.pick(&EXTENDS);
    let (style, style_s) = rng.pick(&STYLES);
    let x1 = x_for(xloc, rng.index());
    // Now and then a vertical or a zero-length line.
    let x2 = if rng.chance(15) {
        x1
    } else {
        x_for(xloc, rng.index())
    };
    let y1 = rng.price();
    let y2 = if rng.chance(15) { y1 } else { rng.price() };
    let l = DrawingLine {
        id,
        pane_id: PANE.to_string(),
        xloc,
        x1,
        y1,
        x2,
        y2,
        extend,
        color: color(rng),
        invisible: rng.chance(10),
        width: 1.0 + rng.below(4) as f64,
        style,
        arrow_left: rng.chance(30),
        arrow_right: rng.chance(30),
        overlay: false,
    };
    let v = json!({
        "id": l.id, "paneId": PANE, "xloc": xloc_s, "x1": l.x1, "y1": l.y1, "x2": l.x2, "y2": l.y2,
        "extend": extend_s, "color": l.color, "invisible": l.invisible, "width": l.width, "style": style_s,
        "arrowLeft": l.arrow_left, "arrowRight": l.arrow_right,
    });
    (l, v)
}

fn boxed(rng: &mut Rng, id: String) -> (DrawingBox, Value) {
    let (xloc, xloc_s) = rng.pick(&XLOCS);
    let (extend, extend_s) = rng.pick(&EXTENDS);
    let (border_style, border_style_s) = rng.pick(&STYLES);
    let (text_size, text_size_s) = rng.pick(&SIZES);
    let (h_align, h_align_s) = rng.pick(&H_ALIGNS);
    let (v_align, v_align_s) = rng.pick(&V_ALIGNS);
    let (font_family, font_family_s) = rng.pick(&FONTS);
    let b = DrawingBox {
        id,
        pane_id: PANE.to_string(),
        xloc,
        left: x_for(xloc, rng.index()),
        top: rng.price(),
        right: x_for(xloc, rng.index()),
        bottom: rng.price(),
        extend,
        bg_color: color(rng),
        border_color: color(rng),
        border_width: rng.below(4) as f64,
        border_style,
        text: rng.pick(&TEXTS).map(str::to_string),
        text_color: color(rng),
        text_size,
        h_align,
        v_align,
        wrap: rng.chance(50),
        font_family,
        bold: rng.chance(25),
        italic: rng.chance(25),
        overlay: false,
    };
    let v = json!({
        "id": b.id, "paneId": PANE, "xloc": xloc_s, "left": b.left, "top": b.top, "right": b.right, "bottom": b.bottom,
        "extend": extend_s, "bgColor": b.bg_color, "borderColor": b.border_color, "borderWidth": b.border_width,
        "borderStyle": border_style_s, "text": b.text, "textColor": b.text_color, "textSize": text_size_s,
        "hAlign": h_align_s, "vAlign": v_align_s, "wrap": b.wrap, "fontFamily": font_family_s, "bold": b.bold, "italic": b.italic,
    });
    (b, v)
}

fn label(
    rng: &mut Rng,
    id: String,
    style: (LabelStyle, &'static str),
    yloc: (LabelYLoc, &'static str),
) -> (DrawingLabel, Value) {
    let (xloc, xloc_s) = rng.pick(&XLOCS);
    let (size, size_s) = rng.pick(&SIZES);
    let (text_align, text_align_s) = rng.pick(&H_ALIGNS);
    let (font_family, font_family_s) = rng.pick(&FONTS);
    let l = DrawingLabel {
        id,
        pane_id: PANE.to_string(),
        xloc,
        x: x_for(xloc, rng.index()),
        y: rng.price(),
        yloc: yloc.0,
        text: rng.pick(&TEXTS).map(str::to_string),
        style: style.0,
        color: color(rng),
        text_color: color(rng),
        size,
        text_align,
        tooltip: if rng.chance(30) {
            Some("Tip text".to_string())
        } else {
            None
        },
        font_family,
        bold: rng.chance(25),
        italic: rng.chance(25),
        no_fill: rng.chance(15),
        overlay: false,
    };
    let v = json!({
        "id": l.id, "paneId": PANE, "xloc": xloc_s, "x": l.x, "y": l.y, "yloc": yloc.1, "text": l.text, "style": style.1,
        "color": l.color, "textColor": l.text_color, "size": size_s, "textAlign": text_align_s, "tooltip": l.tooltip,
        "fontFamily": font_family_s, "bold": l.bold, "italic": l.italic, "noFill": l.no_fill,
    });
    (l, v)
}

fn polyline(rng: &mut Rng, id: String) -> (DrawingPolyline, Value) {
    let (line_style, line_style_s) = rng.pick(&STYLES);
    let count = rng.below(7);
    let mut points = Vec::new();
    let mut points_json = Vec::new();
    for _ in 0..count {
        let (xloc, xloc_s) = rng.pick(&XLOCS);
        let p = PolylinePoint {
            xloc,
            x: x_for(xloc, rng.index()),
            price: rng.price(),
        };
        points_json.push(json!({ "xloc": xloc_s, "x": p.x, "price": p.price }));
        points.push(p);
    }
    let p = DrawingPolyline {
        id,
        pane_id: PANE.to_string(),
        points,
        curved: rng.chance(50),
        closed: rng.chance(50),
        line_color: color(rng),
        fill_color: color(rng),
        line_width: 1.0 + rng.below(3) as f64,
        line_style,
        arrow_left: rng.chance(25),
        arrow_right: rng.chance(25),
        overlay: false,
    };
    let v = json!({
        "id": p.id, "paneId": PANE, "points": points_json, "curved": p.curved, "closed": p.closed,
        "lineColor": p.line_color, "fillColor": p.fill_color, "lineWidth": p.line_width, "lineStyle": line_style_s,
        "arrowLeft": p.arrow_left, "arrowRight": p.arrow_right,
    });
    (p, v)
}

fn tips_json(tips: &[LabelTipRegion]) -> Value {
    Value::Array(
        tips.iter()
            .map(|t| json!({ "left": t.left, "top": t.top, "right": t.right, "bottom": t.bottom, "text": t.text }))
            .collect(),
    )
}

fn scene(rng: &mut Rng, n: usize, theme: &Value, labels_of: Option<usize>) -> Value {
    let mut set = DrawingSet::default();
    let mut lines = Vec::new();
    let mut boxes = Vec::new();
    let mut labels = Vec::new();
    let mut polylines = Vec::new();
    let mut linefills = Vec::new();
    if let Some(style_at) = labels_of {
        // One label style under every vertical placement.
        for (k, yloc) in YLOCS.iter().enumerate() {
            let (l, v) = label(rng, format!("lb{n}-{k}"), LABEL_STYLES[style_at], *yloc);
            set.labels.push(l);
            labels.push(v);
        }
    } else {
        for k in 0..rng.below(4) {
            let (l, v) = line(rng, format!("ln{n}-{k}"));
            set.lines.push(l);
            lines.push(v);
        }
        for k in 0..rng.below(4) {
            let (b, v) = boxed(rng, format!("bx{n}-{k}"));
            set.boxes.push(b);
            boxes.push(v);
        }
        for k in 0..rng.below(4) {
            let style = rng.pick(&LABEL_STYLES);
            let yloc = rng.pick(&YLOCS);
            let (l, v) = label(rng, format!("lb{n}-{k}"), style, yloc);
            set.labels.push(l);
            labels.push(v);
        }
        for k in 0..rng.below(3) {
            let (p, v) = polyline(rng, format!("pl{n}-{k}"));
            set.polylines.push(p);
            polylines.push(v);
        }
        for k in 0..rng.below(3) {
            let (line1, v1) = line(rng, format!("lf{n}-{k}a"));
            let (line2, v2) = line(rng, format!("lf{n}-{k}b"));
            let fill = DrawingLinefill {
                id: format!("lf{n}-{k}"),
                pane_id: PANE.to_string(),
                line1,
                line2,
                color: color(rng),
                overlay: false,
            };
            linefills.push(json!({ "id": fill.id, "paneId": PANE, "line1": v1, "line2": v2, "color": fill.color }));
            set.linefills.push(fill);
        }
    }
    let index_offset = if rng.chance(25) { 5.0 } else { 0.0 };

    let time_to_logical = |ms: f64| ms / MINUTE;
    let bar_at = |logical: f64| {
        let i = logical.round();
        (i >= 0.0 && i < BARS as f64).then(|| {
            let i = i as usize;
            BarHighLow {
                high: 100.0 + (i % 17) as f64,
                low: 90.0 - (i % 11) as f64,
            }
        })
    };
    let deps = LayerDeps {
        time_to_logical: &time_to_logical,
        bar_at: &bar_at,
        theme,
    };
    let x_of = |logical: f64| (logical - 20.0) * 6.0;
    let y_of = |price: f64| price.is_finite().then_some((150.0 - price) * 4.0);

    let mut renderer = DrawingSceneRenderer::new();
    renderer.set_set(set, index_offset);
    let canvas = RecordingCanvas::new();
    renderer.render(&deps, &canvas, W, H, &x_of, &y_of);
    let ranges: Vec<Value> = [(20.0, 186.0), (60.0, 90.0), (300.0, 250.0)]
        .iter()
        .map(|&(from, to)| {
            let range = renderer.price_range(&deps, from, to).map(|r| json!({ "min": r.min, "max": r.max, "aboveMargin": r.above_margin, "belowMargin": r.below_margin }));
            json!({ "from": from, "to": to, "range": range })
        })
        .collect();
    json!({
        "set": { "lines": lines, "boxes": boxes, "labels": labels, "polylines": polylines, "linefills": linefills },
        "indexOffset": index_offset,
        "ops": canvas.to_json(),
        "tips": tips_json(renderer.label_tip_regions()),
        "ranges": ranges,
    })
}

fn table(rng: &mut Rng, n: usize, theme: &Value) -> Value {
    let (position, position_s) = POSITIONS[n % POSITIONS.len()];
    let columns = 1 + rng.below(4);
    let rows = 1 + rng.below(3);
    let mut cells = Vec::new();
    let mut cells_json = Vec::new();
    for _ in 0..rows {
        let mut row = Vec::new();
        let mut row_json = Vec::new();
        for _ in 0..columns {
            if rng.chance(15) {
                row.push(None);
                row_json.push(Value::Null);
                continue;
            }
            let (h_align, h_align_s) = rng.pick(&H_ALIGNS);
            let (v_align, v_align_s) = rng.pick(&V_ALIGNS);
            let (font_family, font_family_s) = rng.pick(&FONTS);
            let (text_size, text_size_json) = if rng.chance(30) {
                let px = 8.0 + rng.below(12) as f64;
                (TableTextSize::Px(px), json!(px))
            } else {
                let (size, size_s) = rng.pick(&SIZES);
                (TableTextSize::Named(size), json!(size_s))
            };
            let cell = TableCell {
                text: rng.pick(&TEXTS).map(str::to_string),
                text_color: color(rng),
                bg_color: color(rng),
                h_align,
                v_align,
                text_size,
                font_family,
                tooltip: if rng.chance(25) {
                    Some("Cell tip".to_string())
                } else {
                    None
                },
                bold: rng.chance(25),
                italic: rng.chance(25),
                width: if rng.chance(25) {
                    Some(rng.below(30) as f64)
                } else {
                    None
                },
                height: if rng.chance(25) {
                    Some(rng.below(20) as f64)
                } else {
                    None
                },
                merged: false,
            };
            row_json.push(json!({
                "text": cell.text, "textColor": cell.text_color, "bgColor": cell.bg_color, "hAlign": h_align_s, "vAlign": v_align_s,
                "textSize": text_size_json, "fontFamily": font_family_s, "tooltip": cell.tooltip, "bold": cell.bold, "italic": cell.italic,
                "width": cell.width, "height": cell.height, "merged": false,
            }));
            row.push(Some(cell));
        }
        cells.push(row);
        cells_json.push(Value::Array(row_json));
    }
    let mut merges = Vec::new();
    if rng.chance(35) && (columns > 1 || rows > 1) {
        let start_col = rng.below(columns as u64);
        let start_row = rng.below(rows as u64);
        merges.push(TableMerge {
            start_col,
            start_row,
            end_col: (start_col + rng.below(2)).min(columns - 1),
            end_row: (start_row + rng.below(2)).min(rows - 1),
        });
    }
    let t = DrawingTable {
        id: format!("tb{n}"),
        pane_id: PANE.to_string(),
        position,
        columns,
        rows,
        bg_color: color(rng),
        frame_color: color(rng),
        frame_width: rng.below(3) as f64,
        border_color: color(rng),
        border_width: rng.below(3) as f64,
        cells,
        merges: merges.clone(),
        overlay: false,
    };
    let canvas = RecordingCanvas::new();
    let mut tips = Vec::new();
    paint_table(
        &canvas,
        &t,
        &TablePaintArgs::new(H, W - 60.0, theme),
        &mut tips,
    );
    json!({
        "table": {
            "id": t.id, "paneId": PANE, "position": position_s, "columns": columns, "rows": rows,
            "bgColor": t.bg_color, "frameColor": t.frame_color, "frameWidth": t.frame_width,
            "borderColor": t.border_color, "borderWidth": t.border_width, "cells": cells_json,
            "merges": merges.iter().map(|m| json!({ "startCol": m.start_col, "startRow": m.start_row, "endCol": m.end_col, "endRow": m.end_row })).collect::<Vec<_>>(),
        },
        "ops": canvas.to_json(),
        "tips": tips_json(&tips),
    })
}

fn main() {
    let theme = json!({ "background": "#131722", "textColor": "#d1d4dc", "fontFamily": "Inter, sans-serif" });
    let mut rng = Rng(4711);
    let mut scenes = Vec::new();
    for n in 0..400 {
        scenes.push(scene(&mut rng, n, &theme, None));
    }
    // Every label style under every vertical placement, a few times over.
    for round in 0..6 {
        for style_at in 0..LABEL_STYLES.len() {
            scenes.push(scene(
                &mut rng,
                1000 + round * 100 + style_at,
                &theme,
                Some(style_at),
            ));
        }
    }
    let tables: Vec<Value> = (0..180).map(|n| table(&mut rng, n, &theme)).collect();
    let out = json!({
        "theme": theme,
        "width": W,
        "height": H,
        "bars": BARS,
        "tablePlotWidth": W - 60.0,
        "scenes": scenes,
        "tables": tables,
    });
    println!("{out}");
}
