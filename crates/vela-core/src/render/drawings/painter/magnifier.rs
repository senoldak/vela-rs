//! The magnifier: an opaque inset showing the chart's market at a finer timeframe.

use crate::drawings::Drawing;
use crate::drawings::types::measure::{Magnifier, magnifier_timeframe_label};
use crate::drawings::{Bounds, DrawingSeriesState, Projector, SeriesUnavailable};
use crate::i18n::{MessageLike, msg};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::price_styles::bar_transform_for;
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::render::core::color::with_alpha;
use crate::render::shared::drawing_geometry::{contrast_color, dash_pattern};

use super::helpers::{px_font, round_rect, set_fill, set_stroke};
use super::{DrawingPainter, DrawingTheme};

/// How the magnifier inset paints each built-in price style (anything else: candles).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Candles,
    Bars,
    Line,
    Area,
}

fn mode_of(style: &str) -> Mode {
    match style {
        "bars" => Mode::Bars,
        "line" | "linemarkers" | "stepline" | "baseline" => Mode::Line,
        "area" | "columns" => Mode::Area,
        _ => Mode::Candles,
    }
}

fn notice(reason: &str) -> MessageLike {
    match reason {
        "loading" => msg("drawing.magnifier.loading", "Loading\u{2026}"),
        "no-data" => msg("drawing.magnifier.noData", "No lower-timeframe data"),
        "too-wide" => msg(
            "drawing.magnifier.tooWide",
            "Area too wide for this timeframe",
        ),
        "none-lower" => msg(
            "drawing.magnifier.noneLower",
            "No lower timeframe available",
        ),
        "not-lower" => msg(
            "drawing.magnifier.notLower",
            "Pick a timeframe lower than the chart's",
        ),
        _ => msg("drawing.magnifier.noSource", "No data source"),
    }
}

impl DrawingPainter {
    /// Placement preview for the magnifier: a dashed rectangle outline only, no backdrop and
    /// no series read, so dragging the area open never kicks a fetch per cursor move.
    pub(super) fn paint_magnifier_ghost(
        &self,
        ctx: &dyn Canvas2d,
        d: &Magnifier,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some((x1, y1, x2, y2)) = d.rect(proj) else {
            return;
        };
        ctx.save();
        set_stroke(ctx, border_color(d, theme));
        ctx.set_line_width(1.0);
        ctx.set_line_dash(&[4.0, 4.0]);
        ctx.stroke_rect(
            js_min(x1, x2),
            js_min(y1, y2),
            (x2 - x1).abs(),
            (y2 - y1).abs(),
        );
        ctx.restore();
    }

    /// Paint a magnifier: an opaque theme-background inset whose interior shows the chart's
    /// market at a finer timeframe: candles at their true time/price positions, clipped to
    /// the rectangle. Bars come through [`Projector::series_in_range`] (cache read;
    /// `Loading` and `Unavailable` states paint a short notice instead). The lower-timeframe
    /// candles shift half a chart bar LEFT of their raw time pixel so each chart candle's
    /// visual cell, centered on its open time, subdivides in place.
    pub(super) fn paint_magnifier(
        &mut self,
        ctx: &dyn Canvas2d,
        d: &Magnifier,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let base = d.base();
        let Some((rx1, ry1, rx2, ry2)) = d.rect(proj) else {
            return;
        };
        let (Some(a), Some(b)) = (base.anchors.first(), base.anchors.get(1)) else {
            return;
        };
        let x0 = js_min(rx1, rx2);
        let x1 = js_max(rx1, rx2);
        let y0 = js_min(ry1, ry2);
        let y1 = js_max(ry1, ry2);
        let w = x1 - x0;
        let h = y1 - y0;

        // Opaque backdrop: the finer candles must read on their own, not blend over the
        // base ones.
        ctx.save();
        ctx.set_global_alpha(1.0);
        set_fill(ctx, &theme.background);
        ctx.fill_rect(x0, y0, w, h);
        ctx.restore();

        let from = js_min(a.time, b.time);
        let to = js_max(a.time, b.time);
        // One chart bar in ms, recovered through the projector (drawings know no bar pitch).
        let chart_bars = proj.bars_between(from, to).unwrap_or(0.0);
        let chart_ms = if chart_bars > 0.0 {
            (to - from) / chart_bars
        } else {
            0.0
        };
        // The request extends one chart bar right: the half-pitch left shift below pulls
        // bars from just past the right edge INTO the rectangle.
        let res = if chart_ms > 0.0 && w > 1.0 && h > 1.0 {
            proj.series_in_range(&d.magnifier.timeframe, from, to + chart_ms)
        } else {
            None
        };

        // `Loading` may carry best-effort partial bars (previously fetched overlapping
        // windows): paint them so a resize keeps its content while the widened fetch runs.
        let (mut series_bars, bar_ms, resolved_tf): (Vec<Ohlcv>, f64, Option<&str>) = match &res {
            Some(DrawingSeriesState::Ready {
                bars,
                timeframe,
                bar_ms,
            }) => (bars.clone(), *bar_ms, Some(timeframe.as_str())),
            Some(DrawingSeriesState::Loading {
                bars,
                timeframe,
                bar_ms,
            }) => (
                bars.clone().unwrap_or_default(),
                *bar_ms,
                Some(timeframe.as_str()),
            ),
            _ => (Vec::new(), 0.0, None),
        };
        if resolved_tf.is_some() && !series_bars.is_empty() {
            // The inset mirrors the CHART's series look: bar-transform styles (Heikin Ashi,
            // and any registered type that declares one) transform the raw fetched bars;
            // line/area/bars restyle the paint (the line family as a line, columns as the
            // filled area they read as); unknown/custom styles fall back to candles. Colors
            // default to the main series' own; a user-picked color wins.
            let look = self.series_look.clone();
            let transform = if look.style != "candles" {
                self.chart_types
                    .as_deref()
                    .and_then(|registry| bar_transform_for(registry, &look.style))
            } else {
                None
            };
            if let Some(t) = transform {
                series_bars = t.full(&series_bars);
            }
            let mode = mode_of(&look.style);
            let half_pitch = (proj.x_of(from + chart_ms) - proj.x_of(from)) / 2.0;
            ctx.save();
            ctx.begin_path();
            ctx.rect(x0, y0, w, h);
            ctx.clip();
            let pick = |own: &str, fallback: &str| {
                if own.is_empty() {
                    fallback.to_string()
                } else {
                    own.to_string()
                }
            };
            if matches!(mode, Mode::Line | Mode::Area) {
                self.paint_magnifier_line(
                    ctx,
                    d,
                    proj,
                    &series_bars,
                    bar_ms,
                    half_pitch,
                    y1,
                    mode == Mode::Area,
                    &pick(&d.magnifier.up_color, &look.line_color),
                );
            } else {
                self.paint_magnifier_bars(
                    ctx,
                    d,
                    proj,
                    &series_bars,
                    bar_ms,
                    half_pitch,
                    (x0, x1),
                    mode == Mode::Bars,
                    &pick(&d.magnifier.up_color, &look.up_color),
                    &pick(&d.magnifier.down_color, &look.down_color),
                );
            }
            ctx.restore();
        } else if let Some(res) = &res {
            let reason = match res {
                DrawingSeriesState::Loading { .. } => "loading",
                DrawingSeriesState::Ready { .. } => "no-data",
                DrawingSeriesState::Unavailable { reason } => match reason {
                    SeriesUnavailable::TooWide => "too-wide",
                    SeriesUnavailable::NoneLower => "none-lower",
                    SeriesUnavailable::NotLower => "not-lower",
                    SeriesUnavailable::NoSource => "no-source",
                },
            };
            self.paint_magnifier_notice(ctx, &self.t(&notice(reason)), x0, y0, w, h, theme);
        }

        // Border on top of the inset content. An unset border color means the theme's
        // contrast ink (white on dark, black on light), resolved per paint so it follows
        // theme switches live; a user-picked color always wins.
        let style = &base.style;
        let line_width = if style.line_width == 0.0 || style.line_width.is_nan() {
            1.0
        } else {
            style.line_width
        };
        ctx.save();
        set_stroke(ctx, border_color(d, theme));
        ctx.set_line_width(line_width);
        ctx.set_line_dash(&dash_pattern(style.line_style, line_width));
        ctx.stroke_rect(x0, y0, w, h);
        ctx.restore();

        // Timeframe chip: OUTSIDE the inset, riding the bottom-left corner (inside the
        // bottom edge only when the rectangle touches the pane's bottom and there is no
        // room below). Chart-background fill so it reads as chrome, not as data. The chip
        // is a live dropdown trigger: a caret marks it clickable, and its painted rect is
        // kept for the interaction layer to hit-test.
        if w > 44.0 {
            let tf = resolved_tf.unwrap_or(&d.magnifier.timeframe);
            let label = self.t(&magnifier_timeframe_label(tf));
            let chip_h = 17.0;
            let gap = 4.0;
            let pane_bottom = match proj.pane_rect(&base.pane_id) {
                Some(pane) => pane.top + pane.height,
                None => proj.height(),
            };
            let below = y1 + gap + chip_h <= pane_bottom;
            let chip_y = if below { y1 + gap } else { y1 - gap - chip_h };
            ctx.save();
            ctx.set_font(&px_font(10.0, &theme.font_family));
            let tw = ctx.measure_text(&label).width;
            let caret_w = 11.0; // room for the dropdown caret after the label
            let chip_w = tw + 12.0 + caret_w;
            round_rect(ctx, x0, chip_y, chip_w, chip_h, 3.0);
            set_fill(ctx, &theme.background);
            ctx.fill();
            set_stroke(ctx, &with_alpha(&theme.text_color, 0.28));
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&[]);
            ctx.stroke();
            set_fill(ctx, &theme.text_color);
            ctx.set_text_align(TextAlign::Left);
            ctx.set_text_baseline(TextBaseline::Middle);
            ctx.fill_text(&label, x0 + 6.0, chip_y + chip_h / 2.0 + 0.5);
            // Caret: a small chevron marking the chip as a dropdown.
            let cxr = x0 + 6.0 + tw + 5.0;
            let cyr = chip_y + chip_h / 2.0;
            set_stroke(ctx, &with_alpha(&theme.text_color, 0.7));
            ctx.set_line_width(1.2);
            ctx.begin_path();
            ctx.move_to(cxr, cyr - 1.5);
            ctx.line_to(cxr + 2.5, cyr + 1.5);
            ctx.line_to(cxr + 5.0, cyr - 1.5);
            ctx.stroke();
            ctx.restore();
            self.chip_rects.insert(
                base.id.clone(),
                Some(Bounds {
                    x: x0,
                    y: chip_y,
                    w: chip_w,
                    h: chip_h,
                }),
            );
        } else {
            self.chip_rects.insert(base.id.clone(), None);
        }
    }

    /// The magnifier's candle/bar loop: each bar's cell spans its open-to-close time
    /// (shifted left by half a chart bar). Candles: wick always, body once the cell is wide
    /// enough to carry one. OHLC bars: the high-low spine with open/close ticks once the
    /// cell has the room.
    #[allow(clippy::too_many_arguments)]
    fn paint_magnifier_bars(
        &self,
        ctx: &dyn Canvas2d,
        d: &Magnifier,
        proj: &dyn Projector,
        bars: &[Ohlcv],
        bar_ms: f64,
        half_pitch: f64,
        (x0, x1): (f64, f64),
        ohlc_bars: bool,
        up_color: &str,
        down_color: &str,
    ) {
        let pane = &d.base().pane_id;
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.0);
        for bar in bars {
            let time = bar.time as f64;
            let cx0 = proj.x_of(time) - half_pitch;
            let cx1 = proj.x_of(time + bar_ms) - half_pitch;
            if cx1 < x0 || cx0 > x1 {
                continue;
            }
            let (Some(y_high), Some(y_low), Some(y_open), Some(y_close)) = (
                proj.y_of(bar.high, pane),
                proj.y_of(bar.low, pane),
                proj.y_of(bar.open, pane),
                proj.y_of(bar.close, pane),
            ) else {
                continue;
            };
            let color = if bar.close >= bar.open {
                up_color
            } else {
                down_color
            };
            let cell_w = cx1 - cx0;
            let cx = (cx0 + cx1) / 2.0;
            set_stroke(ctx, color);
            ctx.begin_path();
            ctx.move_to(cx, y_high);
            ctx.line_to(cx, y_low);
            ctx.stroke();
            if cell_w < 3.0 {
                continue; // too narrow for a body / ticks: the spine carries the bar
            }
            if ohlc_bars {
                let tick = js_max(1.0, cell_w * 0.35);
                ctx.begin_path();
                ctx.move_to(cx - tick, y_open);
                ctx.line_to(cx, y_open);
                ctx.move_to(cx, y_close);
                ctx.line_to(cx + tick, y_close);
                ctx.stroke();
            } else {
                let bw = js_max(1.0, cell_w * 0.7);
                set_fill(ctx, color);
                ctx.fill_rect(
                    cx - bw / 2.0,
                    js_min(y_open, y_close),
                    bw,
                    js_max(1.0, (y_close - y_open).abs()),
                );
            }
        }
    }

    /// The magnifier's line/area rendering: a close polyline through each cell's center
    /// (same half-chart-bar shift as the candles), with an optional translucent fill down to
    /// the rectangle's bottom edge for the area style. Colored like the chart's own line
    /// series.
    #[allow(clippy::too_many_arguments)]
    fn paint_magnifier_line(
        &self,
        ctx: &dyn Canvas2d,
        d: &Magnifier,
        proj: &dyn Projector,
        bars: &[Ohlcv],
        bar_ms: f64,
        half_pitch: f64,
        y_bottom: f64,
        area: bool,
        color: &str,
    ) {
        let pane = &d.base().pane_id;
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for bar in bars {
            let Some(y) = proj.y_of(bar.close, pane) else {
                continue;
            };
            pts.push((proj.x_of(bar.time as f64 + bar_ms / 2.0) - half_pitch, y));
        }
        if pts.len() < 2 {
            return;
        }
        if area {
            ctx.begin_path();
            ctx.move_to(pts[0].0, y_bottom);
            for &(px, py) in &pts {
                ctx.line_to(px, py);
            }
            ctx.line_to(pts[pts.len() - 1].0, y_bottom);
            ctx.close_path();
            set_fill(ctx, &with_alpha(color, 0.15));
            ctx.fill();
        }
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.5);
        set_stroke(ctx, color);
        ctx.begin_path();
        ctx.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            ctx.line_to(p.0, p.1);
        }
        ctx.stroke();
    }

    /// Centered muted notice inside the magnifier rect (loading / unavailable states).
    #[allow(clippy::too_many_arguments)]
    fn paint_magnifier_notice(
        &self,
        ctx: &dyn Canvas2d,
        text: &str,
        x0: f64,
        y0: f64,
        w: f64,
        h: f64,
        theme: &DrawingTheme,
    ) {
        if w < 60.0 || h < 20.0 {
            return;
        }
        ctx.save();
        ctx.begin_path();
        ctx.rect(x0, y0, w, h);
        ctx.clip();
        ctx.set_font(&px_font(11.0, &theme.font_family));
        set_fill(ctx, &with_alpha(&theme.text_color, 0.55));
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.fill_text(text, x0 + w / 2.0, y0 + h / 2.0);
        ctx.restore();
    }
}

/// The magnifier's border ink: its own color, else the theme's contrast ink.
fn border_color<'a>(d: &'a Magnifier, theme: &'a DrawingTheme) -> &'a str {
    let own = d.base().style.line_color.as_str();
    if own.is_empty() {
        contrast_color(Some(&theme.background))
    } else {
        own
    }
}
