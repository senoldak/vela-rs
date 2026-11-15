//! Paints one script `table.new` onto a canvas, mirroring the DOM table overlay's layout
//! semantics: nine-position pane anchoring past the axis, auto/percent cell sizing,
//! merges, collapsed borders, an outer frame, bold/italic/mono text and `\n` line breaks
//! without wrapping. Painting on a canvas is what lets a table ride its indicator's
//! interleave slice and follow the object-tree stacking; cell tooltips survive as
//! hit-rects appended to `tips` (pane space: the caller shifts them).
//!
//! The planning half (font size, merge spans, "has any content") is
//! [`super::table_overlay`].
//!
//! The original reads `ctx.textBaseline` / `ctx.textAlign` before painting and writes
//! them back after; [`Canvas2d`] has no getters, so the caller passes the values the
//! canvas holds on entry in [`TablePaintArgs`] (the context defaults, which is what a
//! freshly saved slice canvas holds).

use std::collections::HashSet;

use serde_json::Value;

use crate::indicators::classics::math::{js_max, js_round};
use crate::model::drawings::{BoxHAlign, BoxVAlign, DrawingTable, TableCell, TablePosition};
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::util::js_number;

use super::drawing_scene_renderer::{LabelTipRegion, drawing_font, theme_text_color, truthy};
use super::table_overlay::{font_px_of, merge_render_plan, table_has_content};

// Mirrors the DOM table overlay's box model: 2px 6px cell padding, a 6px anchor margin,
// and the browser's default ~1.2 line height for `white-space: pre` text.
const PAD_X: f64 = 6.0;
const PAD_Y: f64 = 2.0;
const MARGIN: f64 = 6.0;
const LINE_HEIGHT: f64 = 1.2;

/// Geometry the painter anchors against, all in PANE-RELATIVE pixels (the caller's
/// context is translated to the pane's top and clipped to it).
#[derive(Clone, Copy, Debug)]
pub struct TablePaintArgs<'a> {
    pub pane_height: f64,
    /// Plot-area width (excludes the right price-axis strip): Pine cell widths are
    /// percents of it.
    pub plot_width: f64,
    pub theme: &'a Value,
    /// The canvas's `textAlign` on entry, restored after painting.
    pub entry_text_align: TextAlign,
    /// The canvas's `textBaseline` on entry, restored after painting.
    pub entry_text_baseline: TextBaseline,
}

impl<'a> TablePaintArgs<'a> {
    /// Arguments for a canvas in its default text state (`start`, `alphabetic`).
    pub fn new(pane_height: f64, plot_width: f64, theme: &'a Value) -> Self {
        Self {
            pane_height,
            plot_width,
            theme,
            entry_text_align: TextAlign::Start,
            entry_text_baseline: TextBaseline::Alphabetic,
        }
    }
}

struct CellBox<'a> {
    cell: &'a TableCell,
    r: usize,
    c: usize,
    cs: usize,
    rs: usize,
}

struct TableLayout<'a> {
    col_w: Vec<f64>,
    row_h: Vec<f64>,
    w: f64,
    h: f64,
    boxes: Vec<CellBox<'a>>,
}

/// Paint `t` on `ctx`. Nothing is painted for a table with no cell set via `table.cell()`.
pub fn paint_table(
    ctx: &dyn Canvas2d,
    t: &DrawingTable,
    args: &TablePaintArgs,
    tips: &mut Vec<LabelTipRegion>,
) {
    if !table_has_content(t) {
        return;
    }
    let Some(layout) = layout_table(ctx, t, args) else {
        return;
    };
    if layout.w <= 0.0 || layout.h <= 0.0 {
        return;
    }

    let frame_color = truthy(&t.frame_color);
    let fw = if frame_color.is_some() && t.frame_width > 0.0 {
        t.frame_width
    } else {
        0.0
    };
    let (x, y) = anchor_origin(t.position, layout.w + 2.0 * fw, layout.h + 2.0 * fw, args);
    let x0 = x + fw;
    let y0 = y + fw;

    // Table background behind every cell, then the outer frame stroke around it.
    if let Some(bg) = truthy(&t.bg_color) {
        ctx.set_fill_style(&bg.into());
        ctx.fill_rect(x0, y0, layout.w, layout.h);
    }
    if let (true, Some(frame)) = (fw > 0.0, frame_color) {
        ctx.set_stroke_style(&frame.into());
        ctx.set_line_width(fw);
        ctx.stroke_rect(x + fw / 2.0, y + fw / 2.0, layout.w + fw, layout.h + fw);
    }

    // Column/row pixel origins (prefix sums).
    let mut col_x = vec![0.0];
    for w in &layout.col_w {
        col_x.push(col_x[col_x.len() - 1] + w);
    }
    let mut row_y = vec![0.0];
    for h in &layout.row_h {
        row_y.push(row_y[row_y.len() - 1] + h);
    }

    // Cell backgrounds + text.
    ctx.set_text_baseline(TextBaseline::Middle);
    for bx in &layout.boxes {
        let rx = x0 + col_x[bx.c];
        let ry = y0 + row_y[bx.r];
        let rw = col_x[bx.c + bx.cs] - col_x[bx.c];
        let rh = row_y[bx.r + bx.rs] - row_y[bx.r];
        let cell = bx.cell;
        if let Some(bg) = truthy(&cell.bg_color) {
            ctx.set_fill_style(&bg.into());
            ctx.fill_rect(rx, ry, rw, rh);
        }
        let text = cell.text.as_deref().unwrap_or("");
        if !text.is_empty() {
            let px = font_px_of(cell.text_size);
            ctx.set_font(&cell_font(cell, px, args.theme));
            let fill = cell
                .text_color
                .as_deref()
                .unwrap_or_else(|| theme_text_color(args.theme));
            ctx.set_fill_style(&fill.into());
            let lines: Vec<&str> = text.split('\n').collect();
            let block_h = lines.len() as f64 * px * LINE_HEIGHT;
            let block_top = match cell.v_align {
                BoxVAlign::Top => ry + PAD_Y,
                BoxVAlign::Bottom => ry + rh - PAD_Y - block_h,
                BoxVAlign::Center => ry + (rh - block_h) / 2.0,
            };
            let tx = match cell.h_align {
                BoxHAlign::Left => rx + PAD_X,
                BoxHAlign::Right => rx + rw - PAD_X,
                BoxHAlign::Center => rx + rw / 2.0,
            };
            ctx.set_text_align(match cell.h_align {
                BoxHAlign::Left => TextAlign::Left,
                BoxHAlign::Right => TextAlign::Right,
                BoxHAlign::Center => TextAlign::Center,
            });
            for (i, line) in lines.iter().enumerate() {
                ctx.fill_text(line, tx, block_top + (i as f64 + 0.5) * px * LINE_HEIGHT);
            }
        }
        if let Some(tooltip) = truthy(&cell.tooltip) {
            tips.push(LabelTipRegion {
                left: rx,
                top: ry,
                right: rx + rw,
                bottom: ry + rh,
                text: tooltip.to_string(),
            });
        }
    }
    ctx.set_text_baseline(args.entry_text_baseline);
    ctx.set_text_align(args.entry_text_align);

    // Collapsed cell borders: each grid edge stroked ONCE (adjacent cells share theirs),
    // so translucent border colors don't double up on inner lines.
    if let Some(border) = truthy(&t.border_color)
        && t.border_width > 0.0
    {
        ctx.set_stroke_style(&border.into());
        ctx.set_line_width(t.border_width);
        let mut seen: HashSet<String> = HashSet::new();
        ctx.begin_path();
        let mut edge = |ax: f64, ay: f64, bx: f64, by: f64| {
            let key = format!(
                "{},{},{},{}",
                js_number(ax),
                js_number(ay),
                js_number(bx),
                js_number(by)
            );
            if !seen.insert(key) {
                return;
            }
            ctx.move_to(ax, ay);
            ctx.line_to(bx, by);
        };
        for bx in &layout.boxes {
            let l = js_round(x0 + col_x[bx.c]);
            let r = js_round(x0 + col_x[bx.c + bx.cs]);
            let tp = js_round(y0 + row_y[bx.r]);
            let bt = js_round(y0 + row_y[bx.r + bx.rs]);
            edge(l, tp, r, tp);
            edge(l, bt, r, bt);
            edge(l, tp, l, bt);
            edge(r, tp, r, bt);
        }
        ctx.stroke();
    }
}

/// DOM-table auto layout, approximated: content-sized columns/rows (explicit percent
/// sizes act as minimums, like a `width` on a `pre` cell), spans distributing their
/// deficit evenly, and never-set cells occupying no space.
fn layout_table<'a>(
    ctx: &dyn Canvas2d,
    t: &'a DrawingTable,
    args: &TablePaintArgs,
) -> Option<TableLayout<'a>> {
    let plan = merge_render_plan(t);
    let mut col_w = vec![0.0; t.columns];
    let mut row_h = vec![0.0; t.rows];
    let mut boxes: Vec<CellBox> = Vec::new();
    for r in 0..t.rows {
        for c in 0..t.columns {
            if plan.omit.contains(&(r, c)) {
                continue;
            }
            // Never set via table.cell(): occupies no space.
            let Some(cell) = t
                .cells
                .get(r)
                .and_then(|row| row.get(c))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            let sp = plan.span.get(&(r, c));
            // A malformed region may span zero or fewer; the original's `?? 1` keeps a
            // non-positive span, which no track arithmetic below can use.
            let cs = sp.map_or(1, |s| s.cs).min((t.columns - c) as i64);
            let rs = sp.map_or(1, |s| s.rs).min((t.rows - r) as i64);
            boxes.push(CellBox {
                cell,
                r,
                c,
                cs: cs.max(0) as usize,
                rs: rs.max(0) as usize,
            });
        }
    }
    if boxes.is_empty() {
        return None;
    }

    let size_of = |cell: &TableCell| -> (f64, f64) {
        let px = font_px_of(cell.text_size);
        ctx.set_font(&cell_font(cell, px, args.theme));
        let lines: Vec<&str> = cell.text.as_deref().unwrap_or("").split('\n').collect();
        let mut max_w = 0.0;
        for line in &lines {
            max_w = js_max(max_w, ctx.measure_text(line).width);
        }
        let mut w = max_w.ceil() + 2.0 * PAD_X;
        let mut h = (lines.len() as f64 * px * LINE_HEIGHT).ceil() + 2.0 * PAD_Y;
        if let Some(cw) = cell.width.filter(|v| *v != 0.0 && !v.is_nan()) {
            w = js_max(w, (cw / 100.0) * args.plot_width);
        }
        if let Some(ch) = cell.height.filter(|v| *v != 0.0 && !v.is_nan()) {
            h = js_max(h, (ch / 100.0) * args.pane_height);
        }
        (w, h)
    };

    // Pass 1: non-spanning cells size their own column/row.
    let mut spanning: Vec<(usize, f64, f64)> = Vec::new();
    for (i, bx) in boxes.iter().enumerate() {
        let (w, h) = size_of(bx.cell);
        if bx.cs == 1 {
            col_w[bx.c] = js_max(col_w[bx.c], w);
        }
        if bx.rs == 1 {
            row_h[bx.r] = js_max(row_h[bx.r], h);
        }
        if bx.cs > 1 || bx.rs > 1 {
            spanning.push((i, w, h));
        }
    }
    // Pass 2: a span wider/taller than its tracks spreads the deficit evenly.
    for (i, w, h) in spanning {
        let bx = &boxes[i];
        if bx.cs > 1 {
            let sum: f64 = col_w[bx.c..bx.c + bx.cs].iter().sum();
            if w > sum {
                for cw in &mut col_w[bx.c..bx.c + bx.cs] {
                    *cw += (w - sum) / bx.cs as f64;
                }
            }
        }
        if bx.rs > 1 {
            let sum: f64 = row_h[bx.r..bx.r + bx.rs].iter().sum();
            if h > sum {
                for rh in &mut row_h[bx.r..bx.r + bx.rs] {
                    *rh += (h - sum) / bx.rs as f64;
                }
            }
        }
    }
    let w = col_w.iter().fold(0.0, |a, b| a + b);
    let h = row_h.iter().fold(0.0, |a, b| a + b);
    Some(TableLayout {
        col_w,
        row_h,
        w,
        h,
        boxes,
    })
}

/// Top-left of the table's OUTER box (frame included) for a Pine `position.*` anchor:
/// the same 6px margins and axis-clearing insets the DOM overlay computes.
fn anchor_origin(
    position: TablePosition,
    total_w: f64,
    total_h: f64,
    args: &TablePaintArgs,
) -> (f64, f64) {
    let y = match position {
        TablePosition::TopLeft | TablePosition::TopCenter | TablePosition::TopRight => MARGIN,
        TablePosition::BottomLeft | TablePosition::BottomCenter | TablePosition::BottomRight => {
            args.pane_height - MARGIN - total_h
        }
        _ => args.pane_height / 2.0 - total_h / 2.0,
    };
    let x = match position {
        TablePosition::TopLeft | TablePosition::MiddleLeft | TablePosition::BottomLeft => MARGIN,
        TablePosition::TopRight | TablePosition::MiddleRight | TablePosition::BottomRight => {
            args.plot_width - MARGIN - total_w
        }
        _ => args.plot_width / 2.0 - total_w / 2.0,
    };
    (x, y)
}

fn cell_font(cell: &TableCell, px: f64, theme: &Value) -> String {
    drawing_font(theme, cell.font_family, cell.bold, cell.italic, px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::drawings::{BoxFontFamily, BoxTextSize, TableMerge, TableTextSize};
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    fn theme() -> Value {
        json!({ "fontFamily": "sans-serif", "textColor": "#ddd" })
    }

    fn cell(text: &str) -> TableCell {
        TableCell {
            text: Some(text.to_string()),
            text_color: None,
            bg_color: None,
            h_align: BoxHAlign::Center,
            v_align: BoxVAlign::Center,
            text_size: TableTextSize::Named(BoxTextSize::Normal), // 13px
            font_family: BoxFontFamily::Default,
            tooltip: None,
            bold: false,
            italic: false,
            width: None,
            height: None,
            merged: false,
        }
    }

    fn table(columns: usize, rows: usize, cells: Vec<Vec<Option<TableCell>>>) -> DrawingTable {
        DrawingTable {
            id: "t".into(),
            pane_id: "price".into(),
            position: TablePosition::TopLeft,
            columns,
            rows,
            bg_color: None,
            frame_color: None,
            frame_width: 0.0,
            border_color: None,
            border_width: 0.0,
            cells,
            merges: vec![],
            overlay: false,
        }
    }

    fn paint(t: &DrawingTable) -> (RecordingCanvas, Vec<LabelTipRegion>) {
        let theme = theme();
        let canvas = RecordingCanvas::new();
        let mut tips = Vec::new();
        paint_table(
            &canvas,
            t,
            &TablePaintArgs::new(100.0, 200.0, &theme),
            &mut tips,
        );
        (canvas, tips)
    }

    #[test]
    fn a_full_table_streams_background_frame_cells_text_and_collapsed_borders() {
        let mut c = cell("Hi");
        c.bg_color = Some("#222".into());
        c.text_color = Some("#eee".into());
        c.tooltip = Some("tip".into());
        let mut t = table(1, 1, vec![vec![Some(c)]]);
        t.bg_color = Some("#111".into());
        t.frame_color = Some("#f00".into());
        t.frame_width = 2.0;
        t.border_color = Some("#0f0".into());
        t.border_width = 1.0;
        let (canvas, tips) = paint(&t);

        // 13px "Hi" = 15.6 wide => ceil 16 + 12 = 28; height ceil(15.6) + 4 = 20.
        // Frame 2: the outer box starts at the 6px margin, the cells 2px inside it.
        let block_h = 1.0 * 13.0 * 1.2;
        let block_top = 8.0 + (20.0 - block_h) / 2.0;
        assert_eq!(
            canvas.to_json(),
            json!([
                ["font", "13px sans-serif"],
                ["fillStyle", "#111"],
                ["fillRect", 8.0, 8.0, 28.0, 20.0],
                ["strokeStyle", "#f00"],
                ["lineWidth", 2.0],
                ["strokeRect", 7.0, 7.0, 30.0, 22.0],
                ["textBaseline", "middle"],
                ["fillStyle", "#222"],
                ["fillRect", 8.0, 8.0, 28.0, 20.0],
                ["font", "13px sans-serif"],
                ["fillStyle", "#eee"],
                ["textAlign", "center"],
                [
                    "fillText",
                    "Hi",
                    8.0 + 28.0 / 2.0,
                    block_top + 0.5 * 13.0 * 1.2
                ],
                ["textBaseline", "alphabetic"],
                ["textAlign", "start"],
                ["strokeStyle", "#0f0"],
                ["lineWidth", 1.0],
                ["beginPath"],
                ["moveTo", 8.0, 8.0],
                ["lineTo", 36.0, 8.0],
                ["moveTo", 8.0, 28.0],
                ["lineTo", 36.0, 28.0],
                ["moveTo", 8.0, 8.0],
                ["lineTo", 8.0, 28.0],
                ["moveTo", 36.0, 8.0],
                ["lineTo", 36.0, 28.0],
                ["stroke"],
            ])
        );
        assert_eq!(
            tips,
            vec![LabelTipRegion {
                left: 8.0,
                top: 8.0,
                right: 36.0,
                bottom: 28.0,
                text: "tip".into()
            }]
        );
    }

    #[test]
    fn the_canvas_text_state_on_entry_is_restored() {
        let t = table(1, 1, vec![vec![Some(cell("x"))]]);
        let theme = theme();
        let canvas = RecordingCanvas::new();
        paint_table(
            &canvas,
            &t,
            &TablePaintArgs {
                entry_text_align: TextAlign::Left,
                entry_text_baseline: TextBaseline::Top,
                ..TablePaintArgs::new(100.0, 200.0, &theme)
            },
            &mut Vec::new(),
        );
        let ops = canvas.ops();
        assert_eq!(ops[ops.len() - 2], json!(["textBaseline", "top"]));
        assert_eq!(ops[ops.len() - 1], json!(["textAlign", "left"]));
    }

    #[test]
    fn a_table_without_a_set_cell_paints_nothing() {
        let t = table(2, 2, vec![vec![None, None], vec![None, None]]);
        let (canvas, tips) = paint(&t);
        assert!(canvas.is_empty() && tips.is_empty());
        // merge-absorbed stubs alone are no content either
        let mut stub = cell("x");
        stub.merged = true;
        let t = table(1, 1, vec![vec![Some(stub)]]);
        assert!(paint(&t).0.is_empty());
    }

    #[test]
    fn a_cell_without_text_paints_only_its_background() {
        let mut c = cell("");
        c.bg_color = Some("#abc".into());
        let t = table(1, 1, vec![vec![Some(c)]]);
        let (canvas, _) = paint(&t);
        assert!(canvas.texts().is_empty());
        assert_eq!(
            canvas.calls("fillRect"),
            vec![json!(["fillRect", 6.0, 6.0, 12.0, 20.0])]
        );
    }

    #[test]
    fn the_nine_positions_anchor_to_the_pane_with_six_pixel_margins() {
        // One 28 x 20 cell in a 200 x 100 pane.
        let origin = |position: TablePosition| {
            let mut t = table(1, 1, vec![vec![Some(cell("Hi"))]]);
            t.position = position;
            t.bg_color = Some("#111".into());
            let (canvas, _) = paint(&t);
            let r = canvas.calls("fillRect")[0].clone();
            (r[1].as_f64().unwrap(), r[2].as_f64().unwrap())
        };
        use TablePosition::*;
        assert_eq!(origin(TopLeft), (6.0, 6.0));
        assert_eq!(origin(TopCenter), (200.0 / 2.0 - 28.0 / 2.0, 6.0));
        assert_eq!(origin(TopRight), (200.0 - 6.0 - 28.0, 6.0));
        assert_eq!(origin(MiddleLeft), (6.0, 100.0 / 2.0 - 20.0 / 2.0));
        assert_eq!(
            origin(MiddleCenter),
            (200.0 / 2.0 - 28.0 / 2.0, 100.0 / 2.0 - 20.0 / 2.0)
        );
        assert_eq!(
            origin(MiddleRight),
            (200.0 - 6.0 - 28.0, 100.0 / 2.0 - 20.0 / 2.0)
        );
        assert_eq!(origin(BottomLeft), (6.0, 100.0 - 6.0 - 20.0));
        assert_eq!(
            origin(BottomCenter),
            (200.0 / 2.0 - 28.0 / 2.0, 100.0 - 6.0 - 20.0)
        );
        assert_eq!(
            origin(BottomRight),
            (200.0 - 6.0 - 28.0, 100.0 - 6.0 - 20.0)
        );
    }

    #[test]
    fn percent_sizes_act_as_minimums_of_the_plot_and_pane() {
        let mut c = cell("Hi");
        c.width = Some(50.0); // 50% of the 200 plot width => 100 (> the 28 of the text)
        c.height = Some(10.0); // 10% of the 100 pane height => 10 (< the 20 of the text)
        let mut t = table(1, 1, vec![vec![Some(c)]]);
        t.bg_color = Some("#111".into());
        let (canvas, _) = paint(&t);
        assert_eq!(
            canvas.calls("fillRect"),
            vec![json!(["fillRect", 6.0, 6.0, 100.0, 20.0])]
        );
    }

    #[test]
    fn text_aligns_to_the_cell_edges_with_the_padding_and_stacks_lines_by_1_2() {
        let mk = |h: BoxHAlign, v: BoxVAlign| {
            let mut c = cell("a\nbb");
            c.h_align = h;
            c.v_align = v;
            c.bold = true;
            c.italic = true;
            c.font_family = BoxFontFamily::Monospace;
            c.height = Some(50.0); // a 50px tall cell leaves room to see the vertical alignment
            let mut t = table(1, 1, vec![vec![Some(c)]]);
            t.position = TablePosition::TopLeft;
            let (canvas, _) = paint(&t);
            canvas
        };
        // width: widest line "bb" = 15.6 => 16 + 12 = 28; height max(ceil(31.2) + 4 = 36, 50) = 50.
        let canvas = mk(BoxHAlign::Left, BoxVAlign::Top);
        assert_eq!(
            canvas.to_json(),
            json!([
                ["font", "italic bold 13px monospace"],
                ["textBaseline", "middle"],
                ["font", "italic bold 13px monospace"],
                ["fillStyle", "#ddd"],
                ["textAlign", "left"],
                ["fillText", "a", 6.0 + 6.0, (6.0 + 2.0) + 0.5 * 13.0 * 1.2],
                ["fillText", "bb", 6.0 + 6.0, (6.0 + 2.0) + 1.5 * 13.0 * 1.2],
                ["textBaseline", "alphabetic"],
                ["textAlign", "start"],
            ])
        );
        let canvas = mk(BoxHAlign::Right, BoxVAlign::Bottom);
        let block_h = 2.0 * 13.0 * 1.2;
        let top = 6.0 + 50.0 - 2.0 - block_h;
        assert_eq!(
            canvas.calls("fillText"),
            vec![
                json!(["fillText", "a", 6.0 + 28.0 - 6.0, top + 0.5 * 13.0 * 1.2]),
                json!(["fillText", "bb", 6.0 + 28.0 - 6.0, top + 1.5 * 13.0 * 1.2]),
            ]
        );
    }

    #[test]
    fn a_merged_cell_spreads_its_deficit_over_the_columns_it_spans() {
        // Row 0 is one cell spanning both columns; row 1 has two narrow cells.
        let mut origin = cell("0123456789"); // 10 * 7.8 = 78 => 78 + 12 = 90
        origin.bg_color = Some("#abc".into());
        let mut stub = cell("");
        stub.merged = true;
        let mut t = table(
            2,
            2,
            vec![
                vec![Some(origin), Some(stub)],
                vec![Some(cell("a")), Some(cell("b"))], // 8 + 12 = 20 each
            ],
        );
        t.merges.push(TableMerge {
            start_col: 0,
            start_row: 0,
            end_col: 1,
            end_row: 0,
        });
        t.bg_color = Some("#111".into());
        let (canvas, _) = paint(&t);
        // columns 20 + 20 = 40 < 90: each grows by 25 => 45 + 45; two rows of 20.
        let rects = canvas.calls("fillRect");
        assert_eq!(rects[0], json!(["fillRect", 6.0, 6.0, 90.0, 40.0]));
        assert_eq!(rects[1], json!(["fillRect", 6.0, 6.0, 90.0, 20.0])); // the merged origin
        assert_eq!(canvas.texts(), vec!["0123456789", "a", "b"]);
        let a = canvas.calls("fillText")[1].clone();
        assert_eq!(a[2], json!(6.0 + 45.0 / 2.0)); // centered in the first 45px column
    }

    #[test]
    fn shared_grid_edges_are_stroked_once() {
        let mut t = table(2, 1, vec![vec![Some(cell("a")), Some(cell("b"))]]);
        t.border_color = Some("#0f0".into());
        t.border_width = 1.0;
        let (canvas, _) = paint(&t);
        // top and bottom of each cell, the outer left and right edges, and ONE shared middle.
        assert_eq!(canvas.calls("moveTo").len(), 7);
        assert_eq!(canvas.calls("lineTo").len(), 7);
        assert_eq!(canvas.calls("stroke").len(), 1);
    }

    #[test]
    fn a_frame_needs_both_a_color_and_a_width() {
        let mut t = table(1, 1, vec![vec![Some(cell("a"))]]);
        t.frame_color = Some("#f00".into());
        t.frame_width = 0.0;
        assert!(paint(&t).0.calls("strokeRect").is_empty());
        t.frame_color = None;
        t.frame_width = 2.0;
        assert!(paint(&t).0.calls("strokeRect").is_empty());
    }

    #[test]
    fn cells_that_were_never_set_take_no_space() {
        // A 2 x 2 table where only the bottom-right cell was set sizes to that cell alone,
        // in its own column and row (the other tracks stay 0 wide and tall).
        let mut t = table(2, 2, vec![vec![None, None], vec![None, Some(cell("a"))]]);
        t.bg_color = Some("#111".into());
        let (canvas, _) = paint(&t);
        assert_eq!(
            canvas.calls("fillRect"),
            vec![json!(["fillRect", 6.0, 6.0, 20.0, 20.0])]
        );
        assert_eq!(canvas.texts(), vec!["a"]);
    }

    #[test]
    fn a_numeric_text_size_is_a_pixel_size() {
        let mut c = cell("a");
        c.text_size = TableTextSize::Px(21.0);
        let t = table(1, 1, vec![vec![Some(c)]]);
        let (canvas, _) = paint(&t);
        assert_eq!(canvas.calls("font")[0], json!(["font", "21px sans-serif"]));
    }
}
