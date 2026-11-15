//! Strategy trade markers: the on-chart record of a strategy's order executions. Per fill,
//! one marker UNIT: a direction arrow hugging the bar (buys point up from below the low,
//! sells down from above the high; exit fills carry a cap between the arrow tip and the
//! bar), a text stack reading OUTWARD from the bar (order id, then signed quantity: the
//! quantity is always the outermost line), and a small tick at the exact fill price on
//! the bar's trade-side edge. Multiple fills on one bar stack outward in execution order;
//! at high density units overlap rather than thin out.
//!
//! Canvas painting and measurement, shared across backends (the chrome canvas is always
//! 2d): callers supply coordinate closures, no renderer types cross this boundary. Glyphs
//! and text keep a FIXED pixel size at every zoom, like the axis chrome.

use std::collections::HashMap;

use serde_json::Value;

use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::trades::{TradeExecution, TradeKind, TradeSide};
use crate::palette::{TRADE_EXIT, TRADE_LONG, TRADE_SHORT};
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::util::{js_number, to_fixed};

/// The entry and exit marker colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeMarkerColors {
    pub long: String,
    pub short: String,
    /// The exits' shared color (both directions).
    pub exit: String,
}

/// Display state of the `tradeMarkers` renderer feature (everything on by default).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeMarkersState {
    /// Master visibility of the marker units.
    pub visible: bool,
    /// The order-id/comment text line.
    pub labels: bool,
    /// The signed-quantity text line.
    pub qty: bool,
    /// Entry colors per position side, plus the exits' shared color.
    pub colors: TradeMarkerColors,
}

pub const TRADE_LONG_COLOR: &str = TRADE_LONG;
pub const TRADE_SHORT_COLOR: &str = TRADE_SHORT;
pub const TRADE_EXIT_COLOR: &str = TRADE_EXIT;

impl Default for TradeMarkersState {
    fn default() -> Self {
        Self {
            visible: true,
            labels: true,
            qty: true,
            colors: TradeMarkerColors {
                long: TRADE_LONG_COLOR.to_string(),
                short: TRADE_SHORT_COLOR.to_string(),
                exit: TRADE_EXIT_COLOR.to_string(),
            },
        }
    }
}

pub fn default_trade_markers_state() -> TradeMarkersState {
    TradeMarkersState::default()
}

/// Validated merge of an untrusted partial onto a state: malformed or unknown fields are
/// dropped, so `renderer.set('tradeMarkers', { qty: false })` changes exactly one thing.
pub fn merge_trade_markers_state(base: &TradeMarkersState, patch: &Value) -> TradeMarkersState {
    let empty = serde_json::Map::new();
    let p = patch.as_object().unwrap_or(&empty);
    let c = p.get("colors").and_then(Value::as_object).unwrap_or(&empty);
    let bool_of =
        |key: &str, fallback: bool| p.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    let color = |key: &str, fallback: &str| -> String {
        match c.get(key).and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => fallback.to_string(),
        }
    };
    TradeMarkersState {
        visible: bool_of("visible", base.visible),
        labels: bool_of("labels", base.labels),
        qty: bool_of("qty", base.qty),
        colors: TradeMarkerColors {
            long: color("long", &base.colors.long),
            short: color("short", &base.colors.short),
            exit: color("exit", &base.colors.exit),
        },
    }
}

/// The high and low of the PRICE bar a stack anchors to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarExtent {
    pub high: f64,
    pub low: f64,
}

/// Chart-side resolvers the painter needs (mirrors the drawings' `LayerDeps`).
pub trait TradeMarkerDeps {
    /// Fill time (ms) to fractional logical bar index.
    fn time_to_logical(&self, ms: f64) -> f64;
    /// PRICE bar high/low at a rounded logical index (`None` off the series).
    fn bar_at(&self, logical: f64) -> Option<BarExtent>;
}

/// Autoscale contribution: anchor-bar extremes plus the pixel headroom the stacks need.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeMarkerHints {
    pub min: f64,
    pub max: f64,
    pub above_px: f64,
    pub below_px: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradeMarkerTextStyle {
    pub font_size: f64,
    pub font_family: String,
    pub color: String,
}

// Fixed-pixel geometry, zoom-independent.
const BAR_GAP: f64 = 10.0; // bar extreme to arrow tip
const ARROW_W: f64 = 9.0; // head width
const HEAD_H: f64 = 6.0;
const ARROW_H: f64 = 14.0; // head + stem
const STEM_W: f64 = 3.0;
const CAP_H: f64 = 2.0; // exit cap between the tip and the bar
const CAP_GAP: f64 = 2.0;
const TEXT_GAP: f64 = 3.0; // arrow tail to first text line
const UNIT_GAP: f64 = 6.0; // between stacked units on one bar
const TICK_W: f64 = 6.0; // fill-price tick (points into the bar)
const TICK_H: f64 = 8.0;

struct Unit<'a> {
    exec: &'a TradeExecution,
    /// Text lines outward from the bar: `[label?, qty?]`.
    lines: Vec<String>,
    height: f64,
}

struct BarStack<'a> {
    logical: f64,
    buys: Vec<Unit<'a>>,
    sells: Vec<Unit<'a>>,
}

fn line_height_of(font_size: f64) -> f64 {
    font_size + 4.0
}

/// `+2` / `-0.5`: sign from the side; magnitude trimmed of float noise.
fn qty_text(exec: &TradeExecution) -> Option<String> {
    let qty = exec.qty.filter(|q| q.is_finite())?;
    let magnitude = to_fixed(qty.abs(), 8).parse::<f64>().unwrap_or(f64::NAN);
    let sign = if exec.side == TradeSide::Buy {
        '+'
    } else {
        '-'
    };
    Some(format!("{sign}{}", js_number(magnitude)))
}

fn unit_of<'a>(exec: &'a TradeExecution, state: &TradeMarkersState, line_h: f64) -> Unit<'a> {
    let mut lines = Vec::new();
    if state.labels
        && let Some(label) = exec.label.as_deref().filter(|l| !l.is_empty())
    {
        lines.push(label.to_string());
    }
    if state.qty
        && let Some(q) = qty_text(exec)
    {
        lines.push(q);
    }
    let text_h = if lines.is_empty() {
        0.0
    } else {
        TEXT_GAP + lines.len() as f64 * line_h
    };
    Unit {
        exec,
        lines,
        height: ARROW_H + text_h,
    }
}

/// A map key for a rounded logical index (`-0` and `0` are one key, as in a JS `Map`).
fn logical_key(logical: f64) -> u64 {
    if logical.is_nan() {
        u64::MAX
    } else {
        (logical + 0.0).to_bits()
    }
}

/// Group executions by fill bar, split buys and sells, keeping execution order within a
/// stack (and first-seen order across stacks).
fn stacks_for<'a>(
    trades: &'a [TradeExecution],
    state: &TradeMarkersState,
    deps: &dyn TradeMarkerDeps,
    from: f64,
    to: f64,
    line_h: f64,
) -> Vec<BarStack<'a>> {
    let mut stacks: Vec<BarStack<'a>> = Vec::new();
    let mut by_bar: HashMap<u64, usize> = HashMap::new();
    for exec in trades {
        let logical = js_round(deps.time_to_logical(exec.time as f64));
        if logical < from || logical > to {
            continue;
        }
        let at = *by_bar.entry(logical_key(logical)).or_insert_with(|| {
            stacks.push(BarStack {
                logical,
                buys: Vec::new(),
                sells: Vec::new(),
            });
            stacks.len() - 1
        });
        let unit = unit_of(exec, state, line_h);
        match exec.side {
            TradeSide::Buy => stacks[at].buys.push(unit),
            TradeSide::Sell => stacks[at].sells.push(unit),
        }
    }
    stacks
}

/// Total pixel run of one side's stack, from the bar extreme to the last unit's far edge.
fn stack_extent(units: &[Unit]) -> f64 {
    if units.is_empty() {
        return 0.0;
    }
    let mut px = BAR_GAP;
    for u in units {
        px += u.height;
    }
    px + (units.len() - 1) as f64 * UNIT_GAP
}

/// Price min/max plus above/below pixel margins the visible trade markers need, folded
/// into the price pane's autoscale so a marker stack never clips at the pane edge.
/// Returns `None` when no execution lands in `[from, to]`.
pub fn trades_price_hints(
    trades: &[TradeExecution],
    state: &TradeMarkersState,
    deps: &dyn TradeMarkerDeps,
    from: f64,
    to: f64,
    font_size: f64,
) -> Option<TradeMarkerHints> {
    if trades.is_empty() {
        return None;
    }
    let line_h = line_height_of(font_size);
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut above_px = 0.0;
    let mut below_px = 0.0;
    for stack in stacks_for(trades, state, deps, from.floor(), to.ceil(), line_h) {
        let Some(bar) = deps.bar_at(stack.logical) else {
            continue;
        };
        if bar.low < min {
            min = bar.low;
        }
        if bar.high > max {
            max = bar.high;
        }
        below_px = js_max(below_px, stack_extent(&stack.buys));
        above_px = js_max(above_px, stack_extent(&stack.sells));
    }
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    Some(TradeMarkerHints {
        min,
        max,
        above_px,
        below_px,
    })
}

/// Paint one indicator's executions. Coordinates are pane-relative: `y_of` must already
/// subtract the pane top (the chrome layer translates and clips per pane). `bar_half_px`
/// is half the bar BODY width: the fill-price ticks hug the bar's edges with it.
#[allow(clippy::too_many_arguments)]
pub fn render_trade_markers(
    ctx: &dyn Canvas2d,
    trades: &[TradeExecution],
    state: &TradeMarkersState,
    deps: &dyn TradeMarkerDeps,
    x_of: &dyn Fn(f64) -> f64,
    y_of: &dyn Fn(f64) -> f64,
    text: &TradeMarkerTextStyle,
    width: f64,
    bar_half_px: f64,
) {
    if trades.is_empty() {
        return;
    }
    let line_h = line_height_of(text.font_size);
    let stacks = stacks_for(
        trades,
        state,
        deps,
        f64::NEG_INFINITY,
        f64::INFINITY,
        line_h,
    );
    if stacks.is_empty() {
        return;
    }
    ctx.save();
    ctx.set_font(&format!(
        "{}px {}",
        js_number(text.font_size),
        text.font_family
    ));
    ctx.set_text_align(TextAlign::Center);
    ctx.set_text_baseline(TextBaseline::Middle);
    for stack in &stacks {
        let x = x_of(stack.logical);
        // Cull generously: a unit is about an order id wide, never hundreds of px.
        if x < -150.0 || x > width + 150.0 {
            continue;
        }
        let Some(bar) = deps.bar_at(stack.logical) else {
            continue;
        };
        // Anchor to the bar's VISUAL edges, so an inverted scale keeps units hugging the
        // bar.
        let y_bottom = js_max(y_of(bar.low), y_of(bar.high));
        let y_top = js_min(y_of(bar.low), y_of(bar.high));

        let mut y = y_bottom + BAR_GAP;
        for unit in &stack.buys {
            ctx.set_fill_style(&color_of(unit.exec, &state.colors).into());
            draw_arrow_up(ctx, x, y, unit.exec.kind == TradeKind::Exit);
            draw_fill_tick(ctx, unit.exec.side, x, y_of(unit.exec.price), bar_half_px);
            draw_text_lines(
                ctx,
                &unit.lines,
                x,
                y + ARROW_H + TEXT_GAP + line_h / 2.0,
                line_h,
                &text.color,
            );
            y += unit.height + UNIT_GAP;
        }
        y = y_top - BAR_GAP;
        for unit in &stack.sells {
            ctx.set_fill_style(&color_of(unit.exec, &state.colors).into());
            draw_arrow_down(ctx, x, y, unit.exec.kind == TradeKind::Exit);
            draw_fill_tick(ctx, unit.exec.side, x, y_of(unit.exec.price), bar_half_px);
            draw_text_lines(
                ctx,
                &unit.lines,
                x,
                y - ARROW_H - TEXT_GAP - line_h / 2.0,
                -line_h,
                &text.color,
            );
            y -= unit.height + UNIT_GAP;
        }
    }
    ctx.restore();
}

fn color_of<'a>(exec: &TradeExecution, colors: &'a TradeMarkerColors) -> &'a str {
    if exec.kind == TradeKind::Exit {
        return &colors.exit;
    }
    if exec.side == TradeSide::Buy {
        &colors.long
    } else {
        &colors.short
    }
}

/// Solid up arrow with its tip at `y_tip`: a buy, sitting under the bar, pointing at it.
fn draw_arrow_up(ctx: &dyn Canvas2d, x: f64, y_tip: f64, capped: bool) {
    ctx.begin_path();
    ctx.move_to(x, y_tip);
    ctx.line_to(x - ARROW_W / 2.0, y_tip + HEAD_H);
    ctx.line_to(x + ARROW_W / 2.0, y_tip + HEAD_H);
    ctx.close_path();
    ctx.fill();
    ctx.fill_rect(x - STEM_W / 2.0, y_tip + HEAD_H, STEM_W, ARROW_H - HEAD_H);
    if capped {
        ctx.fill_rect(x - ARROW_W / 2.0, y_tip - CAP_GAP - CAP_H, ARROW_W, CAP_H);
    }
}

/// Solid down arrow with its tip at `y_tip`: a sell, sitting over the bar, pointing at it.
fn draw_arrow_down(ctx: &dyn Canvas2d, x: f64, y_tip: f64, capped: bool) {
    ctx.begin_path();
    ctx.move_to(x, y_tip);
    ctx.line_to(x - ARROW_W / 2.0, y_tip - HEAD_H);
    ctx.line_to(x + ARROW_W / 2.0, y_tip - HEAD_H);
    ctx.close_path();
    ctx.fill();
    ctx.fill_rect(x - STEM_W / 2.0, y_tip - ARROW_H, STEM_W, ARROW_H - HEAD_H);
    if capped {
        ctx.fill_rect(x - ARROW_W / 2.0, y_tip + CAP_GAP, ARROW_W, CAP_H);
    }
}

/// The fill-price tick: a small triangle at the exact fill price, pointing INTO the bar
/// from the trade's side: buys on the left edge, sells on the right.
fn draw_fill_tick(ctx: &dyn Canvas2d, side: TradeSide, x: f64, y_fill: f64, bar_half_px: f64) {
    let edge = if side == TradeSide::Buy {
        x - bar_half_px
    } else {
        x + bar_half_px
    };
    let back = if side == TradeSide::Buy {
        edge - TICK_W
    } else {
        edge + TICK_W
    };
    ctx.begin_path();
    ctx.move_to(edge, y_fill);
    ctx.line_to(back, y_fill - TICK_H / 2.0);
    ctx.line_to(back, y_fill + TICK_H / 2.0);
    ctx.close_path();
    ctx.fill();
}

/// Text stack in the theme's neutral color (never the arrow color), stepping outward.
fn draw_text_lines(
    ctx: &dyn Canvas2d,
    lines: &[String],
    x: f64,
    first_y: f64,
    step: f64,
    color: &str,
) {
    if lines.is_empty() {
        return;
    }
    ctx.set_fill_style(&color.into());
    for (i, line) in lines.iter().enumerate() {
        ctx.fill_text(line, x, first_y + i as f64 * step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    /// Bars live at logical index = time; every bar spans [90, 110].
    struct Deps;

    impl TradeMarkerDeps for Deps {
        fn time_to_logical(&self, ms: f64) -> f64 {
            ms
        }
        fn bar_at(&self, logical: f64) -> Option<BarExtent> {
            (0.0..=50.0).contains(&logical).then_some(BarExtent {
                high: 110.0,
                low: 90.0,
            })
        }
    }

    const FONT: f64 = 11.0; // line height = font size + 4 = 15
    const LINE_H: f64 = FONT + 4.0;

    fn exec(time: i64, side: TradeSide, kind: TradeKind) -> TradeExecution {
        TradeExecution {
            time,
            price: 100.0,
            side,
            kind,
            label: Some("Long".into()),
            qty: Some(2.0),
            trade_id: None,
        }
    }

    fn buy() -> TradeExecution {
        exec(0, TradeSide::Buy, TradeKind::Entry)
    }

    fn sell() -> TradeExecution {
        exec(0, TradeSide::Sell, TradeKind::Entry)
    }

    fn x_of(l: f64) -> f64 {
        100.0 + l * 10.0
    }

    fn y_of(price: f64) -> f64 {
        200.0 - price // higher price = smaller y
    }

    struct Painted {
        canvas: RecordingCanvas,
    }

    struct Text {
        text: String,
        y: f64,
        style: String,
    }

    struct Rect {
        y: f64,
        style: String,
    }

    impl Painted {
        /// Replays the call stream, tracking the fill style as the original's recording
        /// context does.
        fn replay(&self) -> (Vec<Text>, Vec<Rect>, usize) {
            let (mut texts, mut rects, mut fills) = (Vec::new(), Vec::new(), 0);
            let mut style = String::new();
            for op in self.canvas.ops() {
                match op[0].as_str().unwrap_or("") {
                    "fillStyle" => style = op[1].as_str().unwrap_or("").to_string(),
                    "fill" => fills += 1,
                    "fillRect" => rects.push(Rect {
                        y: op[2].as_f64().unwrap_or(f64::NAN),
                        style: style.clone(),
                    }),
                    "fillText" => texts.push(Text {
                        text: op[1].as_str().unwrap_or("").to_string(),
                        y: op[3].as_f64().unwrap_or(f64::NAN),
                        style: style.clone(),
                    }),
                    _ => {}
                }
            }
            (texts, rects, fills)
        }
    }

    fn paint_with(trades: &[TradeExecution], state: &TradeMarkersState) -> Painted {
        let canvas = RecordingCanvas::new();
        let text = TradeMarkerTextStyle {
            font_size: FONT,
            font_family: "sans-serif".into(),
            color: "#ddd".into(),
        };
        render_trade_markers(
            &canvas, trades, state, &Deps, &x_of, &y_of, &text, 800.0, 4.0,
        );
        Painted { canvas }
    }

    fn paint(trades: &[TradeExecution]) -> Painted {
        paint_with(trades, &TradeMarkersState::default())
    }

    fn texts_of(p: &Painted) -> Vec<String> {
        p.replay().0.into_iter().map(|t| t.text).collect()
    }

    fn hints(
        trades: &[TradeExecution],
        state: &TradeMarkersState,
        from: f64,
        to: f64,
    ) -> Option<TradeMarkerHints> {
        trades_price_hints(trades, state, &Deps, from, to, FONT)
    }

    // state merge

    #[test]
    fn defaults_everything_on_with_the_reference_palette() {
        let s = default_trade_markers_state();
        assert!(s.visible && s.labels && s.qty);
        assert_eq!(s.colors.long, TRADE_LONG_COLOR);
        assert_eq!(s.colors.short, TRADE_SHORT_COLOR);
        assert_eq!(s.colors.exit, TRADE_EXIT_COLOR);
    }

    #[test]
    fn merges_a_partial_patch_and_keeps_everything_unnamed() {
        let out = merge_trade_markers_state(
            &default_trade_markers_state(),
            &json!({ "qty": false, "colors": { "exit": "#ffffff" } }),
        );
        assert!(!out.qty);
        assert!(out.visible);
        assert!(out.labels);
        assert_eq!(out.colors.exit, "#ffffff");
        assert_eq!(out.colors.long, TRADE_LONG_COLOR);
    }

    #[test]
    fn drops_malformed_fields() {
        let base = default_trade_markers_state();
        let out = merge_trade_markers_state(
            &base,
            &json!({ "visible": "yes", "qty": 0, "colors": { "long": 42, "short": "  " } }),
        );
        assert_eq!(out, base);
        for junk in [json!(null), json!("x"), json!([1, 2]), json!(7)] {
            assert_eq!(merge_trade_markers_state(&base, &junk), base);
        }
    }

    // autoscale hints

    #[test]
    fn folds_the_anchor_bar_extremes_and_reserves_the_stack_pixels_on_the_right_side() {
        let h = hints(&[buy()], &TradeMarkersState::default(), 0.0, 10.0).unwrap();
        assert_eq!(h.min, 90.0);
        assert_eq!(h.max, 110.0);
        // unit = arrow 14 + gap 3 + two text lines (label + qty) = 47; below = BAR_GAP + 47
        assert_eq!(h.below_px, BAR_GAP + ARROW_H + TEXT_GAP + 2.0 * LINE_H);
        assert_eq!(h.above_px, 0.0);
    }

    #[test]
    fn a_sell_reserves_above_the_highs_and_stacked_fills_accumulate() {
        let state = TradeMarkersState::default();
        let one = hints(&[sell()], &state, 0.0, 10.0).unwrap();
        assert!(one.above_px > 0.0);
        assert_eq!(one.below_px, 0.0);
        let mut exit = exec(0, TradeSide::Sell, TradeKind::Exit);
        exit.label = Some("X".into());
        let two = hints(&[sell(), exit], &state, 0.0, 10.0).unwrap();
        let unit = ARROW_H + TEXT_GAP + 2.0 * LINE_H;
        assert_eq!(two.above_px, BAR_GAP + 2.0 * unit + UNIT_GAP);
    }

    #[test]
    fn hiding_the_text_lines_shrinks_the_reserved_pixels() {
        let state = TradeMarkersState::default();
        let full = hints(&[buy()], &state, 0.0, 10.0).unwrap();
        let no_labels = TradeMarkersState {
            labels: false,
            ..state.clone()
        };
        let bare = TradeMarkersState {
            labels: false,
            qty: false,
            ..state.clone()
        };
        assert_eq!(
            hints(&[buy()], &no_labels, 0.0, 10.0).unwrap().below_px,
            full.below_px - LINE_H
        );
        assert_eq!(
            hints(&[buy()], &bare, 0.0, 10.0).unwrap().below_px,
            BAR_GAP + ARROW_H
        );
    }

    #[test]
    fn hints_are_none_when_nothing_is_in_range_or_off_the_bar_series() {
        let state = TradeMarkersState::default();
        let at = |t| exec(t, TradeSide::Buy, TradeKind::Entry);
        assert_eq!(hints(&[at(30)], &state, 0.0, 10.0), None);
        assert_eq!(hints(&[at(99)], &state, 60.0, 120.0), None);
        assert_eq!(hints(&[], &state, 0.0, 10.0), None);
    }

    // painting

    #[test]
    fn a_buy_paints_under_the_bar_reading_arrow_label_qty_outward() {
        let p = paint(&[buy()]);
        let (texts, _, _) = p.replay();
        assert_eq!(
            texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["Long", "+2"]
        );
        let y_bar_bottom = y_of(90.0); // 110
        assert!(texts[0].y > y_bar_bottom + BAR_GAP + ARROW_H); // below the arrow
        assert!(texts[1].y > texts[0].y); // qty outermost
        assert_eq!(texts[0].style, "#ddd"); // neutral text color, never the arrow color
    }

    #[test]
    fn a_sell_paints_over_the_bar_with_qty_outermost() {
        let mut s = sell();
        s.label = Some("Short".into());
        s.qty = Some(3.0);
        let (texts, _, _) = paint(&[s]).replay();
        assert_eq!(
            texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["Short", "-3"]
        );
        let y_bar_top = y_of(110.0); // 90
        assert!(texts[0].y < y_bar_top - BAR_GAP - ARROW_H);
        assert!(texts[1].y < texts[0].y);
    }

    #[test]
    fn exit_fills_add_the_cap_bar_and_entries_do_not() {
        let (_, entry, _) = paint(&[buy()]).replay();
        assert_eq!(entry.len(), 1); // the arrow stem only
        let (_, exit, _) = paint(&[exec(0, TradeSide::Buy, TradeKind::Exit)]).replay();
        assert_eq!(exit.len(), 2); // stem + cap
        assert!(exit.iter().all(|r| r.style == TRADE_EXIT_COLOR));
    }

    #[test]
    fn colors_long_entries_short_entries_exits() {
        let trades = [
            buy(),
            exec(1, TradeSide::Sell, TradeKind::Entry),
            exec(2, TradeSide::Sell, TradeKind::Exit),
        ];
        let (_, rects, _) = paint(&trades).replay();
        let styles: Vec<&str> = rects.iter().map(|r| r.style.as_str()).collect();
        assert_eq!(
            styles,
            [
                TRADE_LONG_COLOR,
                TRADE_SHORT_COLOR,
                TRADE_EXIT_COLOR,
                TRADE_EXIT_COLOR
            ]
        );
    }

    #[test]
    fn stacked_fills_on_one_bar_step_outward_in_execution_order() {
        let mut a = buy();
        a.label = Some("A".into());
        let mut b = buy();
        b.label = Some("B".into());
        let (_, rects, _) = paint(&[a, b]).replay();
        assert_eq!(rects.len(), 2);
        assert!(rects[1].y > rects[0].y);
    }

    #[test]
    fn respects_the_label_and_qty_toggles() {
        let base = TradeMarkersState::default();
        let labels_off = TradeMarkersState {
            labels: false,
            ..base.clone()
        };
        let qty_off = TradeMarkersState {
            qty: false,
            ..base.clone()
        };
        let both_off = TradeMarkersState {
            labels: false,
            qty: false,
            ..base
        };
        assert_eq!(texts_of(&paint_with(&[buy()], &labels_off)), ["+2"]);
        assert_eq!(texts_of(&paint_with(&[buy()], &qty_off)), ["Long"]);
        assert!(texts_of(&paint_with(&[buy()], &both_off)).is_empty());
    }

    #[test]
    fn trims_float_noise_and_keeps_full_precision_in_the_qty_line() {
        let mut noisy = buy();
        noisy.label = None;
        noisy.qty = Some(0.1 + 0.2);
        assert_eq!(texts_of(&paint(&[noisy.clone()])), ["+0.3"]);
        noisy.qty = Some(0.109884);
        assert_eq!(texts_of(&paint(&[noisy.clone()])), ["+0.109884"]);
        noisy.qty = Some(f64::NAN);
        assert!(texts_of(&paint(&[noisy.clone()])).is_empty());
        noisy.qty = None;
        assert!(texts_of(&paint(&[noisy])).is_empty());
    }

    #[test]
    fn an_empty_label_draws_no_line() {
        let mut e = buy();
        e.label = Some(String::new());
        assert_eq!(texts_of(&paint(&[e])), ["+2"]);
    }

    #[test]
    fn culls_stacks_far_outside_the_plot_and_skips_fills_off_the_bar_series() {
        // x_of(200) = 2100, far past width + 150: culled by x. Time 60 is on-plot but has
        // no bar: skipped.
        let (_, _, far_fills) = paint(&[exec(200, TradeSide::Buy, TradeKind::Entry)]).replay();
        assert_eq!(far_fills, 0);
        let (_, _, off_series) = paint(&[exec(60, TradeSide::Buy, TradeKind::Entry)]).replay();
        assert_eq!(off_series, 0);
    }

    #[test]
    fn no_trades_paint_no_canvas_calls() {
        assert!(paint(&[]).canvas.is_empty());
    }

    #[test]
    fn a_painted_frame_is_wrapped_in_one_save_restore_with_the_text_state_set_first() {
        let names = paint(&[buy()]).canvas.names();
        assert_eq!(&names[..4], ["save", "font", "textAlign", "textBaseline"]);
        assert_eq!(names.last().map(String::as_str), Some("restore"));
        let font = paint(&[buy()]).canvas.calls("font");
        assert_eq!(font[0][1], "11px sans-serif");
    }

    #[test]
    fn fractional_font_sizes_read_like_javascript_numbers() {
        let canvas = RecordingCanvas::new();
        let text = TradeMarkerTextStyle {
            font_size: 12.5,
            font_family: "Inter".into(),
            color: "#ddd".into(),
        };
        render_trade_markers(
            &canvas,
            &[buy()],
            &TradeMarkersState::default(),
            &Deps,
            &x_of,
            &y_of,
            &text,
            800.0,
            4.0,
        );
        assert_eq!(canvas.calls("font")[0][1], "12.5px Inter");
    }
}
