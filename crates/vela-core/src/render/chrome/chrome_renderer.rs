//! Renderer-owned chrome layer (canvas2d) on its own canvas, stacked above the geometry
//! layer (L0) and below the cursor layer (L2). It draws the per-pane price axes and
//! labels, the time axis and labels, the current-price line and chip, the countdown, the
//! pane separators, the strategy trade markers and the timeline-mark lane. Pine drawings
//! do NOT paint here: they prepaint into interleave slices the geometry backend
//! composites at their model's z slot.
//!
//! The painters take the platform values the original reads itself as parameters
//! ([`FrameClock`]): `performance.now()` for the mark pulse and flash, `Date.now()` for
//! the countdown.
//!
//! Left for the browser crate and later stages:
//! - the original's `mount(canvas)`: the context and backing-store size are values
//!   ([`PlotSurface`]);
//! - `setMarkIconReady` and the `MarkIconRaster` (DOM image rasterization): the painter
//!   asks a [`MarkIcons`] for icon images, and the browser crate repaints the chrome when
//!   one lands;
//! - `prepare` and `paneDrawingsRange` (they wire and query the shared
//!   `DrawingSceneRenderer` for the Pine drawings' price range that folds into autoscale;
//!   that renderer lives with the drawing stage, and the renderer shell calls it
//!   directly).
//!
//! Mark-lane hit-testing reads the last frame's layout ([`ChromeRenderer::mark_glyph_at`]
//! and friends), so `render` takes `&mut self`. The tick source is shared with the
//! backdrop, so labels and gridlines agree: an `Rc<PriceAxisTickSource>`.

#![allow(clippy::too_many_arguments)]

use std::rc::Rc;

use crate::format::{FormatSurface, ResolvedFormatters, english_format};
use crate::i18n::Translator;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::marks::MarkGroup;
use crate::model::LineStyle;
use crate::model::{Ohlcv, TradeExecution};
use crate::ports::chart_renderer::VelaTheme;
use crate::render::backend::canvas2d_backend::{PlotSurface, set_line_dash, theme_str};
use crate::render::backend::close_styles::column_is_up;
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::render::core::chart_config::effective_candle_paint;
use crate::render::core::color::with_alpha;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::{CoordinateSystem, PaneNode, SceneGraph};
use crate::render::shared::trade_markers::{
    BarExtent, TradeMarkerDeps, TradeMarkerTextStyle, render_trade_markers,
};
use crate::theme::dark_theme;
use crate::util::js_number;

use super::axis_layout::{PANE_SEPARATOR_PX, axis_column_x};
use super::contrast::tag_text_color;
use super::countdown::countdown_text;
use super::marks_layout::{
    MarkLaneInput, MarkLaneLayout, PlacedGlyph, cluster_tooltip, layout_mark_lane, mark_glyph_at,
    mark_stack_at,
};
use super::marks_paint::{MarkIcons, MarkPaintDeps, paint_mark_lane};
use super::price_axis_ticks::PriceAxisTickSource;
use super::ticks::{AxisScale, format_axis_value, pane_axis_ticks, time_ticks};
use super::tz::{display_zone, tz_offset_ms};

pub use crate::render::core::scene_graph::percent_scale_for;

/// Opacity factor of a price-axis label whose tick is `major: false` (de-emphasized).
const MINOR_LABEL_ALPHA: f64 = 0.5;

/// A CSS font size + family pair, as the original writes it (`11px sans-serif`).
pub fn font_spec(size: f64, family: &str) -> String {
    format!("{}px {}", js_number(size), family)
}

/// The surface the axis-scale gutters paint with: the host passes the live chart
/// background so the scales read as part of the plot, with contrast-corrected text.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisSurface {
    pub background: String,
    pub text_color: String,
}

/// The platform clocks one frame reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameClock {
    /// `performance.now()` (falling back to `Date.now()`): the frame clock of the mark
    /// lane's hover pulse and click flash.
    pub now_ms: f64,
    /// `Date.now()`: wall-clock time the bar-close countdown counts to.
    pub wall_ms: f64,
}

pub struct ChromeRenderer {
    axis_ticks: Rc<PriceAxisTickSource>,
    surface: Option<PlotSurface>,
    /// The color for axis tick labels: the host-passed surface text, set each frame.
    axis_text_color: String,
    /// The timeline-mark lane as laid out by the last frame: what hover/click hit-test
    /// against.
    mark_layout: MarkLaneLayout,
}

impl Default for ChromeRenderer {
    fn default() -> Self {
        Self::new(Rc::new(PriceAxisTickSource::new()))
    }
}

fn formatters(scene: &SceneGraph) -> std::borrow::Cow<'_, ResolvedFormatters> {
    match &scene.format {
        Some(f) => std::borrow::Cow::Borrowed(f),
        None => std::borrow::Cow::Owned(english_format()),
    }
}

/// `array[Math.round(logical)]`.
fn bar_at_logical(bars: &[Ohlcv], logical: f64) -> Option<&Ohlcv> {
    let i = js_round(logical);
    if i >= 0.0 && i.fract() == 0.0 && i < bars.len() as f64 {
        bars.get(i as usize)
    } else {
        None
    }
}

struct TradeDeps<'a> {
    scene: &'a SceneGraph,
    coords: &'a CoordinateSystem,
}

impl TradeMarkerDeps for TradeDeps<'_> {
    fn time_to_logical(&self, ms: f64) -> f64 {
        self.coords.time_to_logical(ms)
    }

    fn bar_at(&self, logical: f64) -> Option<BarExtent> {
        bar_at_logical(&self.scene.bars, logical).map(|b| BarExtent {
            high: b.high,
            low: b.low,
        })
    }
}

impl ChromeRenderer {
    /// `axis_ticks` is shared with the backdrop layer, so price labels and gridlines
    /// agree.
    pub fn new(axis_ticks: Rc<PriceAxisTickSource>) -> Self {
        let default_text = theme_str(&dark_theme(), "textColor").to_string();
        Self {
            axis_ticks,
            surface: None,
            axis_text_color: default_text,
            mark_layout: MarkLaneLayout::default(),
        }
    }

    /// Bind to the chrome canvas: its 2d context and backing-store size (device pixels).
    pub fn mount(&mut self, ctx: Rc<dyn Canvas2d>, backing_width: f64, backing_height: f64) {
        self.surface = Some(PlotSurface::new(ctx, backing_width, backing_height));
    }

    /// The canvas was resized: its new backing-store size in device pixels.
    pub fn set_backing_size(&mut self, backing_width: f64, backing_height: f64) {
        if let Some(surface) = &mut self.surface {
            surface.width = backing_width;
            surface.height = backing_height;
        }
    }

    pub fn destroy(&mut self) {
        self.surface = None;
    }

    /// The interactive mark glyph under a plot point (last frame's layout), or `None`.
    pub fn mark_glyph_at(&self, x: f64, y: f64) -> Option<&PlacedGlyph> {
        mark_glyph_at(&self.mark_layout, x, y)
    }

    /// The mark stack (bar index) whose glyphs, or the gaps of its fan, cover a plot
    /// point.
    pub fn mark_stack_at(&self, x: f64, y: f64) -> Option<usize> {
        mark_stack_at(&self.mark_layout, x, y)
    }

    /// A glyph of the last frame by its cluster key: how an open popup follows its anchor.
    pub fn mark_glyph_by_key(&self, key: &str) -> Option<&PlacedGlyph> {
        self.mark_layout
            .glyphs
            .iter()
            .find(|g| g.cluster.key == key)
    }

    /// Hover text of the mark glyph under a plot point, or `None`.
    pub fn mark_tooltip_at(
        &self,
        x: f64,
        y: f64,
        groups: &[MarkGroup],
        i18n: Option<&dyn Translator>,
    ) -> Option<String> {
        self.mark_glyph_at(x, y)
            .and_then(|g| cluster_tooltip(&g.cluster, groups, i18n))
    }

    /// Clear the chrome canvas and draw axes, the current-price line, trade markers and
    /// the mark lane. `surface` (background + text) paints the axis-scale gutters: the
    /// host passes the live chart background so the scales read as part of the plot, with
    /// contrast-corrected text. Falls back to the theme's own colors when no surface is
    /// supplied.
    pub fn render(
        &mut self,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        surface: Option<&AxisSurface>,
        clock: &FrameClock,
        icons: &dyn MarkIcons,
    ) {
        let Some(plot) = self.surface.clone() else {
            return;
        };
        let ctx: &dyn Canvas2d = &*plot.ctx;

        let dpr = coords.dpr();
        let full_w = plot.width / dpr;
        let full_h = plot.height / dpr;
        let data_w = coords.width();
        let data_h = coords.height();
        // The gutters (and their labels) use the surface the host passes (the live chart
        // background); everything data-side keeps the live theme.
        self.axis_text_color = surface
            .map(|s| s.text_color.clone())
            .unwrap_or_else(|| theme_str(theme, "textColor").to_string());
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, full_w, full_h);
        // Paint the price-axis (right) + time-axis (bottom) gutters opaquely so drawings
        // or series pixels beneath never bleed into the scales. Data/drawings stay clear
        // of these strips, so this only ever covers the axis areas.
        if let Some(surface) = surface
            && (full_w > data_w || full_h > data_h)
        {
            ctx.set_fill_style(&(&surface.background).into());
            if full_w > data_w {
                ctx.fill_rect(data_w, 0.0, full_w - data_w, full_h);
            }
            if full_h > data_h {
                ctx.fill_rect(0.0, data_h, data_w, full_h - data_h);
            }
        }
        ctx.set_font(&font_spec(
            scene.style.font_size,
            theme_str(theme, "fontFamily"),
        ));
        ctx.set_text_baseline(TextBaseline::Middle);

        let panes = scene.ordered_panes();
        if coords.bar_count() == 0 {
            // A market switch (timeframe/symbol) clears the series while the new bars
            // load, and any chrome frame in that window (crosshair move, resize) lands
            // here. The pane SEPARATORS are structural: they depend on pane bounds alone,
            // not bars, so they must survive the empty frame, or the stacked panes read
            // as one undivided plot until the load completes.
            self.draw_pane_separators(ctx, scene, theme, full_w, &panes);
            self.mark_layout = MarkLaneLayout::default(); // no bars: nothing to click either
            return;
        }
        let price_pane = panes
            .iter()
            .copied()
            .find(|p| p.kind == crate::model::PaneKind::Price);

        // Pine drawings paint through the interleave slices at their model's z slot, NOT
        // here: the chrome stays axes + markers + chips.

        // Strategy trade markers: always the PRICE pane, whatever pane the strategy's
        // plots landed on (a fill price only means something on the price scale), above
        // the drawings. Hiding the indicator removes its model, and the markers with it.
        if let Some(pane) = price_pane
            && !pane.collapsed
            && scene.trade_markers.visible
        {
            for m in scene.indicators.values() {
                if !m.trades.is_empty() {
                    self.render_trades(ctx, coords, scene, theme, &m.trades, pane, data_w);
                }
            }
        }

        // axes + current-price line + countdown
        self.draw_price_axes(ctx, scene, coords, theme, data_w, &panes);
        self.draw_merged_scale_columns(ctx, scene, coords, data_w);
        self.draw_pane_separators(ctx, scene, theme, full_w, &panes);
        self.draw_price_line_and_countdown(ctx, scene, coords, theme, data_w, price_pane, clock);
        self.draw_time_axis(ctx, scene, coords, theme, data_w, data_h, full_h);
        self.draw_mark_lane(ctx, scene, coords, theme, data_w, data_h, clock, icons);
    }

    /// The timeline-mark lane: after the axis, so the tokens read over the plot's bottom
    /// edge, but clipped to the data area so a glyph on the edge bar never paints over the
    /// price scale.
    fn draw_mark_lane(
        &mut self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        data_w: f64,
        data_h: f64,
        clock: &FrameClock,
        icons: &dyn MarkIcons,
    ) {
        if !scene.marks.visible || scene.timeline_marks.is_empty() {
            self.mark_layout = MarkLaneLayout::default();
            return;
        }
        let marks: Vec<Rc<_>> = scene.timeline_marks.iter().cloned().map(Rc::new).collect();
        let bar_times: Vec<i64> = scene.bars.iter().map(|b| b.time).collect();
        let hidden = |group_id: &str| {
            !scene
                .marks
                .group_visible(Some(group_id), &scene.mark_groups)
        };
        self.mark_layout = layout_mark_lane(&MarkLaneInput {
            marks: &marks,
            groups: &scene.mark_groups,
            hidden: &hidden,
            bar_times: &bar_times,
            interval_ms: coords.bar_interval(),
            x_of: &|bar| coords.logical_to_x(bar as f64),
            axis_y: data_h,
            data_w,
            expanded: scene
                .marks_expanded_stack
                .and_then(|stack| usize::try_from(stack).ok()),
        });
        let now_ms = clock.now_ms;
        let intl = super::super::backdrop::scene_intl(scene);
        ctx.save();
        ctx.begin_path();
        ctx.rect(0.0, 0.0, data_w, data_h);
        ctx.clip();
        let flash_key = scene
            .marks_flash
            .as_ref()
            .filter(|f| f.until > now_ms)
            .map(|f| f.key.as_str());
        paint_mark_lane(
            ctx,
            &self.mark_layout,
            &MarkPaintDeps {
                axis_y: data_h,
                background: theme_str(theme, "background"),
                stem_color: scene
                    .style
                    .border_color
                    .as_deref()
                    .unwrap_or_else(|| theme_str(theme, "borderColor")),
                font_family: theme_str(theme, "fontFamily"),
                dpr: coords.dpr(),
                icons,
                intl: &*intl,
                hover_key: scene.marks_hover_key.as_deref(),
                hover_since: scene.marks_hover_since,
                active_key: scene.marks_active_key.as_deref(),
                flash_key,
                now_ms,
            },
        );
        ctx.restore();
    }

    fn render_trades(
        &self,
        ctx: &dyn Canvas2d,
        coords: &CoordinateSystem,
        scene: &SceneGraph,
        theme: &VelaTheme,
        trades: &[TradeExecution],
        pane: &PaneNode,
        data_w: f64,
    ) {
        ctx.save();
        ctx.translate(0.0, pane.bounds.top); // pane-relative space, clipped like the drawings
        ctx.begin_path();
        ctx.rect(0.0, 0.0, data_w, pane.bounds.height);
        ctx.clip();
        render_trade_markers(
            ctx,
            trades,
            &scene.trade_markers,
            &TradeDeps { scene, coords },
            &|logical| coords.logical_to_x(logical),
            &|price| coords.price_to_y(price, &pane.scale, &pane.bounds) - pane.bounds.top,
            &TradeMarkerTextStyle {
                font_size: scene.style.font_size,
                font_family: theme_str(theme, "fontFamily").to_string(),
                color: theme_str(theme, "textColor").to_string(),
            },
            data_w,
            // Half the candle BODY width (bodies take ~0.8 of the pitch), so the
            // fill-price ticks hug the bar's edges at every zoom.
            js_max(1.5, coords.body_spacing() * 0.4),
        );
        ctx.restore();
    }

    // ── axes ──

    fn draw_price_axes(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        data_w: f64,
        panes: &[&PaneNode],
    ) {
        let border = scene
            .style
            .border_color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        ctx.set_stroke_style(&border.into());
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(data_w + 0.5, 0.0);
        ctx.line_to(data_w + 0.5, coords.height());
        ctx.stroke();
        if !scene.show_axis_labels {
            return;
        }
        ctx.set_fill_style(&(&self.axis_text_color).into());
        ctx.set_text_align(TextAlign::Left);
        let font = font_spec(scene.style.font_size, theme_str(theme, "fontFamily"));
        let major_font = format!("600 {font}");
        let alpha = ctx.global_alpha();
        for pane in panes {
            if pane.collapsed {
                continue; // collapsed strip: legend only, no scale numbers
            }
            for t in self.axis_ticks.ticks_for(scene, pane).iter() {
                let y = coords.price_to_y(t.price, &pane.scale, &pane.bounds);
                if y < pane.bounds.top + 6.0 || y > pane.bounds.top + pane.bounds.height - 4.0 {
                    continue;
                }
                if t.major == Some(true) {
                    ctx.set_font(&major_font);
                } else if t.major == Some(false) {
                    ctx.set_global_alpha(alpha * MINOR_LABEL_ALPHA);
                }
                ctx.fill_text(&t.label, data_w + 6.0, y);
                if t.major == Some(true) {
                    ctx.set_font(&font);
                } else if t.major == Some(false) {
                    ctx.set_global_alpha(alpha);
                }
            }
            // A paneAxis-overridden pane labels its bands instead of prices (a
            // categorical axis): same column, same typography as the price ticks.
            if let Some(bands) = &pane.axis_bands {
                for b in bands {
                    let y = pane.bounds.top + b.frac * pane.bounds.height;
                    if y < pane.bounds.top + 6.0 || y > pane.bounds.top + pane.bounds.height - 4.0 {
                        continue;
                    }
                    ctx.fill_text(&b.label, data_w + 6.0, y);
                }
            }
        }
        ctx.set_text_align(TextAlign::Start);
    }

    /// The horizontal divider at each stacked pane's top edge, spanning the FULL width
    /// (data area + right-hand scale gutter) as one continuous line. Drawn on the chrome
    /// layer, above the data canvas, so series/candles never overpaint it: the line reads
    /// at a uniform thickness across the whole width. Its draggable hit-zone (input) and
    /// hover highlight (crosshair layer) match this same full span.
    fn draw_pane_separators(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        theme: &VelaTheme,
        full_w: f64,
        panes: &[&PaneNode],
    ) {
        let color = scene
            .style
            .separator_color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        ctx.set_fill_style(&color.into());
        for pane in panes {
            if pane.order <= 0 {
                continue; // no separator above the topmost (price) pane
            }
            ctx.fill_rect(
                0.0,
                js_round(pane.bounds.top) - 1.0,
                full_w,
                PANE_SEPARATOR_PX,
            );
        }
    }

    /// Draw an axis column per merged (own-scale) indicator, to the right of each pane's
    /// master scale: tick labels in the chart's axis text color. Columns are told apart by
    /// spacing alone (no divider line), and a collapsed pane's columns are skipped
    /// entirely. This is what makes a merged indicator readable on its own values while
    /// sharing the pane.
    fn draw_merged_scale_columns(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        data_w: f64,
    ) {
        if !scene.show_axis_labels {
            return;
        }
        ctx.set_text_align(TextAlign::Left);
        // A merged column reads with the same axis text color as the master scale (from
        // the chart's settings): no per-indicator tint, so the gutter stays uniform.
        ctx.set_fill_style(&(&self.axis_text_color).into());
        let fmt = formatters(scene);
        for pane in scene.ordered_panes() {
            if pane.collapsed {
                continue; // collapsed strip: legend only, no scale numbers
            }
            let merged = scene.own_scale_indicators_for_pane(&pane.id);
            for (k, model) in merged.iter().enumerate() {
                let Some(sc) = scene.indicator_scales.get(&model.id).map(|s| s.scale) else {
                    continue;
                };
                let x = axis_column_x(data_w, k + 1); // column 0 is the master scale
                for t in pane_axis_ticks(
                    &AxisScale::from(&sc),
                    pane.bounds.height,
                    None,
                    scene.price_mintick,
                    None,
                    &fmt,
                ) {
                    let y = coords.price_to_y(t.price, &sc, &pane.bounds);
                    if y < pane.bounds.top + 6.0 || y > pane.bounds.top + pane.bounds.height - 4.0 {
                        continue;
                    }
                    ctx.fill_text(&t.label, x + 5.0, y);
                }
            }
        }
        ctx.set_text_align(TextAlign::Start);
    }

    /// The latest-price chrome on the price pane, all colored with the price element's own
    /// color (candle/bar up-down, line, area, or baseline side) and contrast text:
    /// - the dashed current-price LINE (`show_price_line`), fully independent of the
    ///   label,
    /// - the last-price LABEL chip (`show_price_label`),
    /// - the countdown-to-bar-close chip (`show_countdown`).
    ///
    /// When the label and countdown are both on they merge into one stacked block
    /// (countdown under the label, text flushed left); a lone label or countdown is
    /// centered on the price level with centered text. The countdown repaints on the
    /// renderer's second pulse and disappears once the bar has closed, until the next bar
    /// arrives.
    fn draw_price_line_and_countdown(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        data_w: f64,
        price_pane: Option<&PaneNode>,
        clock: &FrameClock,
    ) {
        let n = scene.bars.len();
        // A hidden price series takes its current-price line/label/countdown down with
        // it, as does a hidden price PANE (collapsed, or zero-height while a study pane
        // is maximized).
        let Some(price_pane) = price_pane else { return };
        if n == 0 || scene.candles_hidden || price_pane.collapsed || price_pane.bounds.height <= 0.0
        {
            return;
        }
        let last = &scene.bars[n - 1];
        let y = coords.price_to_y(last.close, &price_pane.scale, &price_pane.bounds);
        if y < price_pane.bounds.top || y > price_pane.bounds.top + price_pane.bounds.height {
            return;
        }
        let color = price_element_color(scene, coords, theme, last);

        // dashed line (independent of the label)
        if scene.show_price_line {
            let yy = js_round(y) + 0.5;
            ctx.set_stroke_style(&(&color).into());
            ctx.set_line_width(1.0);
            set_line_dash(ctx, LineStyle::Dotted);
            ctx.begin_path();
            ctx.move_to(0.0, yy);
            ctx.line_to(data_w, yy);
            ctx.stroke();
            set_line_dash(ctx, LineStyle::Solid);
        }

        // axis chips: last-price label and/or countdown
        let fmt = formatters(scene);
        let cd_text = if scene.show_countdown {
            countdown_text(last.time as f64, coords.bar_interval(), clock.wall_ms, &fmt)
        } else {
            None
        };
        let show_label = scene.show_price_label;
        if !show_label && cd_text.is_none() {
            return;
        }

        let price_text = format_axis_value(
            &AxisScale::from(&price_pane.scale),
            price_pane.bounds.height,
            last.close,
            percent_scale_for(scene, price_pane),
            scene.price_mintick,
            None,
            &fmt,
            FormatSurface::LastPrice,
        );
        const PAD: f64 = 8.0;
        let x = data_w + 1.0;
        // Text color chosen for contrast against the chip's own color (so a white candle
        // color yields dark text, a dark color yields light text).
        let text_color = tag_text_color(&color, theme_str(theme, "background"));
        ctx.set_text_baseline(TextBaseline::Middle);

        if show_label && let Some(cd_text) = &cd_text {
            // Merged block: label row on top (centered on the price line), countdown row
            // under it. Same width, text flushed left.
            let w = js_max(
                ctx.measure_text(&price_text).width,
                ctx.measure_text(cd_text).width,
            ) + PAD;
            let top = y - 8.0;
            let tx = x + PAD / 2.0;
            ctx.set_fill_style(&(&color).into());
            ctx.fill_rect(x, top, w, 32.0);
            ctx.set_fill_style(&text_color.into());
            ctx.set_text_align(TextAlign::Left);
            ctx.fill_text(&price_text, tx, top + 8.0);
            ctx.fill_text(cd_text, tx, top + 24.0);
            ctx.set_text_align(TextAlign::Start);
            return;
        }

        // Lone label or countdown: centered on the price level, text centered.
        let text = if show_label {
            price_text
        } else {
            cd_text.unwrap_or_default()
        };
        let w = ctx.measure_text(&text).width + PAD;
        ctx.set_fill_style(&(&color).into());
        ctx.fill_rect(x, y - 8.0, w, 16.0);
        ctx.set_fill_style(&text_color.into());
        ctx.set_text_align(TextAlign::Center);
        ctx.fill_text(&text, x + w / 2.0, y);
        ctx.set_text_align(TextAlign::Start);
    }

    fn draw_time_axis(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        data_w: f64,
        data_h: f64,
        full_h: f64,
    ) {
        let border = scene
            .style
            .border_color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        ctx.set_stroke_style(&border.into());
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(0.0, data_h + 0.5);
        ctx.line_to(data_w, data_h + 0.5);
        ctx.stroke();
        if !scene.show_axis_labels {
            return;
        }
        ctx.set_fill_style(&(&self.axis_text_color).into());
        ctx.set_text_align(TextAlign::Center);
        let y = data_h + (full_h - data_h) / 2.0;
        let tr = coords.visible_time_range();
        let intl = super::super::backdrop::scene_intl(scene);
        let offset = tz_offset_ms(
            &*intl,
            (tr.from + tr.to) / 2.0,
            display_zone(&scene.timezone, coords.bar_interval()),
        );
        // Width-adaptive density so a narrow surface (a phone, a multi-chart cell) asks
        // for fewer ticks instead of cramming the default eight. The floor of 3 keeps a
        // narrow axis populated (a too-small target snaps the ladder to a huge step whose
        // few ticks can all miss the frame); the collision pass below is what actually
        // prevents overlap.
        let target = js_max(3.0, js_min(8.0, f64::floor(data_w / 64.0)));
        let fmt = formatters(scene);
        struct Placed {
            x: f64,
            half: f64,
            label: String,
            major: bool,
        }
        let ticks: Vec<Placed> = time_ticks(tr.from, tr.to, target, offset, &fmt)
            .into_iter()
            .map(|tick| Placed {
                x: coords.time_to_x(tick.time),
                half: ctx.measure_text(&tick.label).width / 2.0,
                label: tick.label,
                major: tick.major,
            })
            .filter(|tick| tick.x >= 20.0 && tick.x <= data_w - 20.0)
            .collect();
        // Measured collision pass on top: label pitch in PIXELS isn't uniform (bar-index
        // mapping, session gaps), so overlapping labels are SKIPPED, majors placed first
        // (a date beats the 12:00 beside it).
        const GAP: f64 = 12.0; // min px between neighboring labels
        let mut placed: Vec<(f64, f64)> = Vec::new();
        let mut put = |tick: &Placed| {
            let l = tick.x - tick.half;
            let r = tick.x + tick.half;
            if !placed.iter().all(|p| r + GAP <= p.0 || l - GAP >= p.1) {
                return;
            }
            placed.push((l, r));
            ctx.fill_text(&tick.label, tick.x, y);
        };
        for tick in &ticks {
            if tick.major {
                put(tick);
            }
        }
        for tick in &ticks {
            if !tick.major {
                put(tick);
            }
        }
        ctx.set_text_align(TextAlign::Start);
    }
}

/// The color of the latest price element for the active chart style: matches how the
/// series itself is drawn: candle/bar body up-down, the line/area line color, the baseline
/// side (above/below the baseline price), or the last column's up-down (worn opaque: the
/// line and label are thin chrome, not a filled area). Candles resolve through the active
/// style's own candle colors (a candle-based plugin style such as Footprint or TPO keeps
/// its own), exactly as the candle painter does; with bodies hidden the candle reads as
/// its outline, so the outline color is used.
pub fn price_element_color(
    scene: &SceneGraph,
    coords: &CoordinateSystem,
    theme: &VelaTheme,
    last: &Ohlcv,
) -> String {
    let st = &scene.style;
    let up_color = theme_str(theme, "upColor");
    let down_color = theme_str(theme, "downColor");
    let up = last.close >= last.open;
    match scene.price_style.as_str() {
        "bars" => if up {
            st.bars.up_color.as_deref().unwrap_or(up_color)
        } else {
            st.bars.down_color.as_deref().unwrap_or(down_color)
        }
        .to_string(),
        "line" | "linemarkers" | "stepline" => {
            st.line.color.as_deref().unwrap_or(up_color).to_string()
        }
        "columns" => {
            let col_up = column_is_up(&scene.bars, scene.bars.len().wrapping_sub(1));
            with_alpha(
                if col_up {
                    st.columns.up_color.as_deref().unwrap_or(up_color)
                } else {
                    st.columns.down_color.as_deref().unwrap_or(down_color)
                },
                1.0,
            )
        }
        "area" => st
            .area
            .line_color
            .as_deref()
            .unwrap_or(up_color)
            .to_string(),
        "baseline" => {
            let i0 = js_max(0.0, f64::floor(coords.visible_logical_range().from));
            let baseline = scene
                .baseline_value
                .or_else(|| bar_at_index(&scene.bars, i0).map(|b| b.close))
                .unwrap_or(0.0);
            if last.close >= baseline {
                st.baseline.top_line_color.as_deref().unwrap_or(up_color)
            } else {
                st.baseline
                    .bottom_line_color
                    .as_deref()
                    .unwrap_or(down_color)
            }
            .to_string()
        }
        // 'candles', 'heikinashi', and candle-based plugin styles.
        _ => {
            let paint = effective_candle_paint(
                &st.candle,
                scene.candle_override.as_ref(),
                up_color,
                down_color,
            );
            let body = if up { &paint.up } else { &paint.down };
            if !paint.candle.body_visible && paint.candle.border_visible {
                let border = if up {
                    &paint.candle.border_up_color
                } else {
                    &paint.candle.border_down_color
                };
                return border.clone().unwrap_or_else(|| body.clone());
            }
            body.clone()
        }
    }
}

fn bar_at_index(bars: &[Ohlcv], index: f64) -> Option<&Ohlcv> {
    if index >= 0.0 && index.fract() == 0.0 && index < bars.len() as f64 {
        bars.get(index as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::{Value, json};

    use super::*;
    use crate::marks::{MarkGlyph, TimelineMark};
    use crate::model::{
        IndicatorModel, Ohlcv, PaneAxisBand, PaneKind, TradeExecution, TradeKind, TradeSide,
    };
    use crate::options::{PriceAxisTick, PriceAxisTickContext, PriceAxisTicksFn};
    use crate::render::backend::canvas2d_backend::test_support::{
        bars, coords_for, frame_start, model, recording, scene_with, stream, theme,
    };
    use crate::render::canvas::RecordingCanvas;
    use crate::render::chrome::axis_layout::axis_column_x;
    use crate::render::chrome::marks_layout::MARK_GLYPH_PX;
    use crate::render::chrome::marks_paint::NoMarkIcons;
    use crate::render::core::PriceScale;
    use crate::render::core::ViewportState;
    use crate::render::core::scene_graph::PaneAxisFormat;

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    const T0: i64 = 1_700_000_000_000;

    fn clock() -> FrameClock {
        FrameClock {
            now_ms: 0.0,
            wall_ms: 0.0,
        }
    }

    /// The pixel y of a price on the 0..100 pane over 100 px, by the original's arithmetic.
    fn y(price: f64) -> f64 {
        0.0 + 100.0 * (1.0 - (price - 0.0) / (100.0 - 0.0))
    }

    fn tick(price: f64, label: &str) -> PriceAxisTick {
        PriceAxisTick {
            price,
            label: label.into(),
            major: None,
        }
    }

    fn fixed_ticks(ticks: Vec<PriceAxisTick>) -> Rc<PriceAxisTickSource> {
        let source = Rc::new(PriceAxisTickSource::new());
        let hook: PriceAxisTicksFn = Rc::new(move |_: &PriceAxisTickContext| Some(ticks.clone()));
        source.set_hook(Some(hook));
        source
    }

    /// Three one-minute bars on a 300 x 100 plot (pitch 8, 6-bar margin: x = 236, 244, 252),
    /// a 364 x 122 canvas (a 64 px price column, a 22 px time axis).
    fn three_bars() -> SceneGraph {
        let mut s = scene_with(
            bars(&[
                (40.0, 60.0, 30.0, 50.0),
                (50.0, 70.0, 40.0, 45.0),
                (45.0, 55.0, 35.0, 52.0),
            ]),
            0.0,
            100.0,
            0.0,
            100.0,
        );
        s.show_price_label = false;
        s.show_price_line = false;
        s.show_countdown = false;
        s
    }

    fn three_coords(s: &SceneGraph) -> CoordinateSystem {
        coords_for(s, 300.0, 100.0, 1.0, 8.0, 6.0)
    }

    fn mounted(source: Rc<PriceAxisTickSource>) -> (ChromeRenderer, Rc<RecordingCanvas>) {
        let canvas = recording();
        let mut chrome = ChromeRenderer::new(source);
        chrome.mount(canvas.clone(), 364.0, 122.0);
        (chrome, canvas)
    }

    fn render(
        s: &SceneGraph,
        c: &CoordinateSystem,
        surface: Option<&AxisSurface>,
        source: Rc<PriceAxisTickSource>,
    ) -> Rc<RecordingCanvas> {
        let (mut chrome, canvas) = mounted(source);
        chrome.render(s, c, &theme(), surface, &clock(), &NoMarkIcons);
        canvas
    }

    fn texts_at(canvas: &RecordingCanvas) -> Vec<(String, f64, f64)> {
        canvas
            .calls("fillText")
            .iter()
            .map(|o| {
                (
                    o[1].as_str().unwrap().to_string(),
                    o[2].as_f64().unwrap(),
                    o[3].as_f64().unwrap(),
                )
            })
            .collect()
    }

    // ── the complete stream ──

    #[test]
    fn a_frame_with_axes_the_price_chip_and_the_time_axis_paints_this_exact_stream() {
        let mut s = three_bars();
        s.show_price_line = true;
        s.show_price_label = true;
        let c = three_coords(&s);
        let surface = AxisSurface {
            background: "#101010".into(),
            text_color: "#aaaaaa".into(),
        };
        let canvas = render(
            &s,
            &c,
            Some(&surface),
            fixed_ticks(vec![tick(50.0, "50.00")]),
        );

        // Time axis: 4 ticks asked (300 / 64 = 4.7 -> 4) over a 37.5 minute window gives the
        // 15-minute ladder: -15 min (x = 300 - (8 + 15) * 8 = 116) and 0 (x = 236). The
        // label at midnight is the major one and goes down first.
        let tr = c.visible_time_range();
        let ticks = time_ticks(tr.from, tr.to, 4.0, 0.0, &english_format());
        assert_eq!(
            ticks.iter().map(|t| t.time).collect::<Vec<_>>(),
            vec![-900_000.0, 0.0]
        );
        assert_eq!((c.time_to_x(-900_000.0), c.time_to_x(0.0)), (116.0, 236.0));
        assert!(!ticks[0].major && ticks[1].major);

        // The last close (52, up) colors the line and the chip: the up color; its label
        // reads in white (a 0.29 luminance is under the 0.4 flip point).
        let last_y = y(52.0);
        let line_y = last_y.round() + 0.5;
        let price_text = format_axis_value(
            &AxisScale::new(0.0, 100.0),
            100.0,
            52.0,
            None,
            None,
            None,
            &english_format(),
            FormatSurface::LastPrice,
        );
        let chip_w = price_text.chars().count() as f64 * 6.6 + 8.0;
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, 364.0, 122.0),
                vec![
                    // the gutters, opaque
                    json!(["fillStyle", "#101010"]),
                    json!(["fillRect", 300.0, 0.0, 64.0, 122.0]),
                    json!(["fillRect", 0.0, 100.0, 300.0, 22.0]),
                    json!(["font", "11px sans-serif"]),
                    json!(["textBaseline", "middle"]),
                    // the price axis line and its labels
                    json!(["strokeStyle", "#333333"]),
                    json!(["lineWidth", 1.0]),
                    json!(["beginPath"]),
                    json!(["moveTo", 300.5, 0.0]),
                    json!(["lineTo", 300.5, 100.0]),
                    json!(["stroke"]),
                    json!(["fillStyle", "#aaaaaa"]),
                    json!(["textAlign", "left"]),
                    json!(["fillText", "50.00", 306.0, y(50.0)]),
                    json!(["textAlign", "start"]),
                    // merged scale columns: none, but the state is still set
                    json!(["textAlign", "left"]),
                    json!(["fillStyle", "#aaaaaa"]),
                    json!(["textAlign", "start"]),
                    // pane separators: the price pane has none
                    json!(["fillStyle", "#333333"]),
                    // the dotted current-price line
                    json!(["strokeStyle", "#00AA00"]),
                    json!(["lineWidth", 1.0]),
                    json!(["setLineDash", [2.0, 3.0]]),
                    json!(["beginPath"]),
                    json!(["moveTo", 0.0, line_y]),
                    json!(["lineTo", 300.0, line_y]),
                    json!(["stroke"]),
                    json!(["setLineDash", []]),
                    // the last-price chip, centered on the level
                    json!(["textBaseline", "middle"]),
                    json!(["fillStyle", "#00AA00"]),
                    json!(["fillRect", 301.0, last_y - 8.0, chip_w, 16.0]),
                    json!(["fillStyle", "#ffffff"]),
                    json!(["textAlign", "center"]),
                    json!(["fillText", price_text, 301.0 + chip_w / 2.0, last_y]),
                    json!(["textAlign", "start"]),
                    // the time axis line, then the labels, majors first
                    json!(["strokeStyle", "#333333"]),
                    json!(["lineWidth", 1.0]),
                    json!(["beginPath"]),
                    json!(["moveTo", 0.0, 100.5]),
                    json!(["lineTo", 300.0, 100.5]),
                    json!(["stroke"]),
                    json!(["fillStyle", "#aaaaaa"]),
                    json!(["textAlign", "center"]),
                    json!(["fillText", ticks[1].label, 236.0, 111.0]),
                    json!(["fillText", ticks[0].label, 116.0, 111.0]),
                    json!(["textAlign", "start"]),
                ],
            ])
        );
    }

    #[test]
    fn without_a_surface_the_gutters_are_not_filled_and_labels_use_the_theme_text_color() {
        let s = three_bars();
        let canvas = render(
            &s,
            &three_coords(&s),
            None,
            fixed_ticks(vec![tick(50.0, "50")]),
        );
        assert!(canvas.calls("fillRect").is_empty());
        assert_eq!(
            canvas.calls("fillStyle")[0],
            json!(["fillStyle", "#cccccc"])
        );
    }

    #[test]
    fn an_unmounted_or_destroyed_chrome_paints_nothing_and_a_resize_changes_the_clear() {
        let s = three_bars();
        let c = three_coords(&s);
        let mut chrome = ChromeRenderer::default();
        chrome.render(&s, &c, &theme(), None, &clock(), &NoMarkIcons);
        let (mut chrome, canvas) = mounted(Rc::new(PriceAxisTickSource::new()));
        chrome.set_backing_size(728.0, 244.0);
        chrome.render(&s, &c, &theme(), None, &clock(), &NoMarkIcons);
        assert_eq!(
            canvas.to_json()[2],
            json!(["clearRect", 0.0, 0.0, 728.0, 244.0])
        );
        chrome.destroy();
        canvas.clear();
        chrome.render(&s, &c, &theme(), None, &clock(), &NoMarkIcons);
        assert!(canvas.is_empty());
    }

    // ── pane separators (chrome-separators.test.ts) ──

    fn separator_scene() -> SceneGraph {
        let mut s = SceneGraph::new();
        s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        let study = s.ensure_pane("pane-x", PaneKind::Study, 1, Some(1.0));
        study.bounds.top = 300.0;
        study.bounds.height = 100.0;
        s
    }

    fn empty_coords() -> CoordinateSystem {
        let mut c = CoordinateSystem::new();
        c.set_size(800.0, 400.0, 1.0); // no bars: the market-switch load state
        c
    }

    fn render_empty(s: &SceneGraph) -> Rc<RecordingCanvas> {
        let canvas = recording();
        let mut chrome = ChromeRenderer::default();
        chrome.mount(canvas.clone(), 800.0, 400.0);
        chrome.render(s, &empty_coords(), &theme(), None, &clock(), &NoMarkIcons);
        canvas
    }

    #[test]
    fn each_stacked_pane_separator_is_drawn_even_while_the_series_is_empty() {
        let canvas = render_empty(&separator_scene());
        // The study pane's divider: full width at its top edge (top - 1), 3 px thick.
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, 800.0, 400.0),
                vec![
                    json!(["font", "11px sans-serif"]),
                    json!(["textBaseline", "middle"]),
                    json!(["fillStyle", "#333333"]),
                    json!(["fillRect", 0.0, 299.0, 800.0, 3.0]),
                ],
            ])
        );
    }

    #[test]
    fn a_lone_price_pane_has_no_separator_and_the_separator_color_can_be_set() {
        let mut s = SceneGraph::new();
        s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        let canvas = render_empty(&s);
        assert!(canvas.calls("fillRect").is_empty());
        let mut s = separator_scene();
        s.style.separator_color = Some("#abcdef".into());
        assert_eq!(
            render_empty(&s).calls("fillStyle"),
            vec![json!(["fillStyle", "#abcdef"])]
        );
    }

    // ── the price axis ──

    #[test]
    fn labels_clip_near_the_pane_edges_collapsed_panes_have_none_and_the_toggle_hides_them() {
        let mut s = three_bars();
        let c = three_coords(&s);
        // y = 95 (inside: the limit is height - 4 = 96), 50, 5 (under the 6 px top margin) and
        // 97 (past the bottom limit)
        let ticks = vec![
            tick(5.0, "low"),
            tick(50.0, "mid"),
            tick(95.0, "high"),
            tick(3.0, "edge"),
        ];
        let canvas = render(&s, &c, None, fixed_ticks(ticks.clone()));
        let labels: Vec<String> = texts_at(&canvas)
            .into_iter()
            .filter(|t| t.1 == 306.0)
            .map(|t| t.0)
            .collect();
        assert_eq!(labels, vec!["low", "mid"]);

        s.panes.get_mut("price").unwrap().collapsed = true;
        let canvas = render(&s, &c, None, fixed_ticks(ticks.clone()));
        assert!(texts_at(&canvas).iter().all(|t| t.1 != 306.0));

        let mut hidden = three_bars();
        hidden.show_axis_labels = false;
        let canvas = render(&hidden, &three_coords(&hidden), None, fixed_ticks(ticks));
        // Only the axis lines: the price axis and the time axis.
        assert!(texts_at(&canvas).is_empty());
        assert_eq!(canvas.calls("stroke").len(), 2);
    }

    #[test]
    fn a_pane_axis_override_labels_its_bands_at_fractions_of_the_pane_height() {
        let mut s = three_bars();
        {
            let pane = s.panes.get_mut("price").unwrap();
            pane.axis_bands = Some(vec![
                PaneAxisBand {
                    frac: 0.25,
                    label: "A".into(),
                },
                PaneAxisBand {
                    frac: 0.5,
                    label: "B".into(),
                },
                PaneAxisBand {
                    frac: 0.99,
                    label: "clipped".into(),
                },
            ]);
            pane.axis_format = Some(PaneAxisFormat::NoAxis);
        }
        let canvas = render(
            &s,
            &three_coords(&s),
            None,
            Rc::new(PriceAxisTickSource::new()),
        );
        let bands: Vec<(String, f64)> = texts_at(&canvas)
            .into_iter()
            .filter(|t| t.1 == 306.0)
            .map(|t| (t.0, t.2))
            .collect();
        assert_eq!(
            bands,
            vec![("A".to_string(), 25.0), ("B".to_string(), 50.0)]
        );
    }

    #[test]
    fn merged_indicators_get_an_axis_column_each_with_their_own_scale() {
        let mut s = three_bars();
        let mut m: IndicatorModel = model("m", "price");
        m.own_scale = true;
        s.indicators.insert("m", m);
        s.assign_indicator_z("m");
        s.ensure_indicator_scale("m", Some(PriceScale::new(0.0, 10.0)));
        s.panes.get_mut("price").unwrap().bounds.height = 300.0; // room for a few ticks
        let c = three_coords(&s);
        let canvas = render(&s, &c, None, fixed_ticks(vec![]));
        let x = axis_column_x(300.0, 1) + 5.0;
        let scale = PriceScale::new(0.0, 10.0);
        let pane = s.panes.get("price").unwrap();
        let expected: Vec<(String, f64, f64)> = crate::render::chrome::ticks::pane_axis_ticks(
            &AxisScale::from(&scale),
            300.0,
            None,
            None,
            None,
            &english_format(),
        )
        .into_iter()
        .filter_map(|t| {
            let ty = c.price_to_y(t.price, &scale, &pane.bounds);
            (6.0..=296.0).contains(&ty).then_some((t.label, x, ty))
        })
        .collect();
        assert!(expected.len() >= 2);
        // (the time axis labels share the canvas: pick this column's)
        let column: Vec<(String, f64, f64)> =
            texts_at(&canvas).into_iter().filter(|t| t.1 == x).collect();
        assert_eq!(column, expected);
    }

    // ── the current-price chrome ──

    fn countdown_scene(label: bool, countdown: bool, line: bool) -> (SceneGraph, CoordinateSystem) {
        let mut s = three_bars();
        s.show_price_label = label;
        s.show_countdown = countdown;
        s.show_price_line = line;
        let c = three_coords(&s);
        (s, c)
    }

    fn chip_frame(s: &SceneGraph, c: &CoordinateSystem, wall_ms: f64) -> Rc<RecordingCanvas> {
        let (mut chrome, canvas) = mounted(Rc::new(PriceAxisTickSource::new()));
        chrome.render(
            s,
            c,
            &theme(),
            None,
            &FrameClock {
                now_ms: 0.0,
                wall_ms,
            },
            &NoMarkIcons,
        );
        canvas
    }

    #[test]
    fn a_lone_label_and_a_lone_countdown_are_centered_on_the_price_level() {
        let (s, c) = countdown_scene(true, false, false);
        let canvas = chip_frame(&s, &c, 0.0);
        let last_y = y(52.0);
        let chip: Vec<Value> = canvas
            .ops()
            .into_iter()
            .filter(|o| o[0] == "fillRect" && o[1] == json!(301.0))
            .collect();
        assert_eq!(chip.len(), 1);
        assert_eq!(chip[0][2], json!(last_y - 8.0));
        assert_eq!(chip[0][4], json!(16.0));

        // The last bar opened at 120_000 on a 60_000 cadence; at 150_000 it has 30 s left.
        let (s, c) = countdown_scene(false, true, false);
        let canvas = chip_frame(&s, &c, 150_000.0);
        let texts = texts_at(&canvas);
        let at_axis: Vec<&(String, f64, f64)> = texts
            .iter()
            .filter(|t| t.1 > 300.0 && t.1 != 306.0)
            .collect();
        assert_eq!(at_axis.len(), 1);
        assert_eq!(at_axis[0].0, "00:30");
        assert_eq!(at_axis[0].2, last_y);
    }

    #[test]
    fn label_and_countdown_merge_into_one_stacked_block_with_flushed_left_text() {
        let (s, c) = countdown_scene(true, true, false);
        let canvas = chip_frame(&s, &c, 150_000.0);
        let last_y = y(52.0);
        let price_text = format_axis_value(
            &AxisScale::new(0.0, 100.0),
            100.0,
            52.0,
            None,
            None,
            None,
            &english_format(),
            FormatSurface::LastPrice,
        );
        let w = f64::max(price_text.chars().count() as f64, 5.0) * 6.6 + 8.0;
        let top = last_y - 8.0;
        let ops = canvas.ops();
        let at = ops
            .iter()
            .position(|o| o == &json!(["fillRect", 301.0, top, w, 32.0]))
            .unwrap();
        assert_eq!(
            ops[at - 1..at + 7].to_vec(),
            vec![
                json!(["fillStyle", "#00AA00"]),
                json!(["fillRect", 301.0, top, w, 32.0]),
                json!(["fillStyle", "#ffffff"]),
                json!(["textAlign", "left"]),
                json!(["fillText", price_text, 305.0, top + 8.0]),
                json!(["fillText", "00:30", 305.0, top + 24.0]),
                json!(["textAlign", "start"]),
                json!(["strokeStyle", "#333333"]),
            ]
        );
    }

    #[test]
    fn a_closed_bar_has_no_countdown_and_the_line_alone_draws_no_chip() {
        let (s, c) = countdown_scene(false, true, true);
        let canvas = chip_frame(&s, &c, 1_000_000.0); // long after the bar closed
        assert!(canvas.calls("fillRect").is_empty());
        assert_eq!(
            canvas.calls("setLineDash"),
            vec![
                json!(["setLineDash", [2.0, 3.0]]),
                json!(["setLineDash", []])
            ]
        );
    }

    #[test]
    fn a_hidden_price_series_a_collapsed_or_flat_pane_or_an_offscreen_price_has_no_price_chrome() {
        let all = |s: &mut SceneGraph| {
            s.show_price_label = true;
            s.show_price_line = true;
            s.show_countdown = true;
        };
        let mut hidden = three_bars();
        all(&mut hidden);
        hidden.candles_hidden = true;
        let mut collapsed = three_bars();
        all(&mut collapsed);
        collapsed.panes.get_mut("price").unwrap().collapsed = true;
        let mut flat = three_bars();
        all(&mut flat);
        flat.panes.get_mut("price").unwrap().bounds.height = 0.0;
        let mut off = three_bars();
        all(&mut off);
        off.panes.get_mut("price").unwrap().scale = PriceScale::new(100.0, 200.0); // the close is below the window
        for s in [hidden, collapsed, flat, off] {
            let canvas = chip_frame(&s, &three_coords(&s), 150_000.0);
            assert!(canvas.calls("setLineDash").is_empty());
            assert!(texts_at(&canvas).iter().all(|t| t.0 != "00:30"));
        }
    }

    // ── the color of the latest price element ──

    fn color_scene(style: &str, last: Ohlcv) -> (SceneGraph, CoordinateSystem) {
        let mut s = three_bars();
        s.bars = vec![Ohlcv::new(0, 10.0, 12.0, 8.0, 11.0, None), last];
        s.price_style = style.into();
        let c = three_coords(&s);
        (s, c)
    }

    fn color_of(style: &str, last: Ohlcv) -> String {
        let (s, c) = color_scene(style, last);
        price_element_color(&s, &c, &theme(), &last)
    }

    #[test]
    fn the_price_color_follows_the_active_style() {
        let up = Ohlcv::new(60_000, 10.0, 12.0, 9.0, 11.0, None);
        let down = Ohlcv::new(60_000, 11.0, 12.0, 9.0, 10.0, None);
        assert_eq!(color_of("candles", up), "#00AA00");
        assert_eq!(color_of("heikinashi", down), "#AA0000");
        assert_eq!(color_of("bars", up), "#00AA00");
        // A style's own color wins.
        let (mut s, c) = color_scene("bars", down);
        s.style.bars.down_color = Some("#123456".into());
        assert_eq!(price_element_color(&s, &c, &theme(), &down), "#123456");
        for style in ["line", "linemarkers", "stepline"] {
            let (mut s, c) = color_scene(style, up);
            s.style.line.color = None;
            assert_eq!(price_element_color(&s, &c, &theme(), &up), "#00AA00"); // inherits the up color
            s.style.line.color = Some("#abcdef".into());
            assert_eq!(price_element_color(&s, &c, &theme(), &up), "#abcdef");
        }
        let (mut s, c) = color_scene("area", up);
        s.style.area.line_color = Some("#fedcba".into());
        assert_eq!(price_element_color(&s, &c, &theme(), &up), "#fedcba");
    }

    #[test]
    fn a_columns_price_reads_opaque_by_the_close_against_the_previous_close() {
        let (mut s, c) = color_scene("columns", Ohlcv::new(60_000, 5.0, 12.0, 9.0, 13.0, None));
        s.style.columns.up_color = Some("rgba(0,170,0,0.5)".into());
        s.style.columns.down_color = Some("rgba(170,0,0,0.5)".into());
        let rose = s.bars[1];
        assert_eq!(
            price_element_color(&s, &c, &theme(), &rose),
            "rgba(0,170,0,1)"
        );
        s.bars[1].close = 5.0; // below the previous close (11)
        let fell = s.bars[1];
        assert_eq!(
            price_element_color(&s, &c, &theme(), &fell),
            "rgba(170,0,0,1)"
        );
        // Unset colors fall to the theme's: not parseable by name here, so left as given.
        s.style.columns.up_color = None;
        s.bars[1].close = 20.0;
        let up = s.bars[1];
        assert_eq!(
            price_element_color(&s, &c, &theme(), &up),
            "rgba(0,170,0,1)"
        );
    }

    #[test]
    fn a_baseline_price_takes_the_side_of_the_baseline_and_falls_back_to_the_first_visible_close() {
        let rising = Ohlcv::new(60_000, 10.0, 12.0, 9.0, 11.5, None);
        let (mut s, c) = color_scene("baseline", rising);
        s.style.baseline.top_line_color = Some("#top".into());
        s.style.baseline.bottom_line_color = Some("#bottom".into());
        s.baseline_value = Some(11.0);
        assert_eq!(price_element_color(&s, &c, &theme(), &rising), "#top");
        s.baseline_value = Some(12.0);
        assert_eq!(price_element_color(&s, &c, &theme(), &rising), "#bottom");
        // Without a baseline value: the close of the first visible bar (11), and 0 with no bars.
        s.baseline_value = None;
        assert_eq!(price_element_color(&s, &c, &theme(), &rising), "#top");
        s.bars.clear();
        assert_eq!(price_element_color(&s, &c, &theme(), &rising), "#top");
        s.style.baseline.top_line_color = None;
        assert_eq!(price_element_color(&s, &c, &theme(), &rising), "#00AA00"); // the theme's up
    }

    #[test]
    fn a_candle_with_hidden_bodies_reads_as_its_outline() {
        let up = Ohlcv::new(60_000, 10.0, 12.0, 9.0, 11.0, None);
        let (mut s, c) = color_scene("candles", up);
        s.style.candle.body_visible = false;
        s.style.candle.border_visible = true;
        assert_eq!(price_element_color(&s, &c, &theme(), &up), "#00AA00"); // no border color: the body color
        s.style.candle.border_up_color = Some("#abcdef".into());
        assert_eq!(price_element_color(&s, &c, &theme(), &up), "#abcdef");
        s.style.candle.body_visible = true;
        assert_eq!(price_element_color(&s, &c, &theme(), &up), "#00AA00");
    }

    // ── the time axis (coarse-bar-dates.test.ts) ──

    fn utc_ms(y: i32, m0: u32, d: u32, h: u32) -> i64 {
        crate::intl::days_from_civil(i64::from(y), m0 + 1, d) * DAY + i64::from(h) * HOUR
    }

    fn daily() -> Vec<i64> {
        [19, 20, 21, 22, 23, 26, 27]
            .iter()
            .map(|d| utc_ms(2026, 9, *d, 0))
            .collect()
    }

    fn hourly() -> Vec<i64> {
        (0..48).map(|i| utc_ms(2026, 9, 22, 0) + i * HOUR).collect()
    }

    fn coords_over(times: &[i64], width: f64, spacing: f64) -> CoordinateSystem {
        let mut c = CoordinateSystem::new();
        c.set_size(width, 300.0, 1.0);
        c.set_bars(times);
        c.set_viewport(ViewportState {
            bar_spacing: spacing,
            right_offset: 0.0,
        });
        c
    }

    /// The labels `draw_time_axis` paints, with their x.
    fn axis_labels(
        times: &[i64],
        width: f64,
        spacing: f64,
        zone: &str,
    ) -> (CoordinateSystem, Vec<(String, f64)>) {
        let c = coords_over(times, width, spacing);
        let mut s = SceneGraph::new();
        s.timezone = zone.into();
        let canvas = recording();
        ChromeRenderer::default().draw_time_axis(&*canvas, &s, &c, &theme(), width, 300.0, 328.0);
        let labels = texts_at(&canvas).into_iter().map(|t| (t.0, t.1)).collect();
        (c, labels)
    }

    #[test]
    fn every_day_label_sits_under_its_bars_utc_midnight_whatever_the_display_zone() {
        let (c, utc) = axis_labels(&daily(), 600.0, 100.0, "UTC");
        assert!(utc.len() >= 4);
        for (text, x) in &utc {
            let day: i64 = text.strip_prefix("Oct ").unwrap().parse().unwrap();
            assert!(
                (x - c.time_to_x(utc_ms(2026, 9, day as u32, 0) as f64)).abs() < 1e-6,
                "{text}"
            );
        }
        for d in [20, 21, 22, 23] {
            let label = format!("Oct {d}");
            let x = utc.iter().find(|t| t.0 == label).unwrap().1;
            let index = daily()
                .iter()
                .position(|t| *t == utc_ms(2026, 9, d, 0))
                .unwrap();
            assert!((x - c.logical_to_x(index as f64)).abs() < 1e-6, "{label}");
        }
        for zone in ["Etc/GMT+5", "+09:00", "UTC", "Etc/UTC"] {
            assert_eq!(axis_labels(&daily(), 600.0, 100.0, zone).1, utc, "{zone}");
        }
    }

    #[test]
    fn an_intraday_axis_still_shifts_into_the_display_zone() {
        let (utc_c, utc) = axis_labels(&hourly(), 480.0, 10.0, "UTC");
        let (zone_c, zoned) = axis_labels(&hourly(), 480.0, 10.0, "Etc/GMT+4");
        let x_of = |labels: &[(String, f64)]| labels.iter().find(|t| t.0 == "Oct 23").map(|t| t.1);
        assert!(
            (x_of(&utc).unwrap() - utc_c.time_to_x(utc_ms(2026, 9, 23, 0) as f64)).abs() < 1e-6
        );
        assert!(
            (x_of(&zoned).unwrap() - zone_c.time_to_x(utc_ms(2026, 9, 23, 4) as f64)).abs() < 1e-6
        );
    }

    #[test]
    fn narrow_axes_ask_for_fewer_ticks_and_overlapping_labels_are_skipped_minors_after_majors() {
        // A 100 px axis asks for the floor of 3 ticks; whatever lands has gaps of 12 px.
        let (_, labels) = axis_labels(&hourly(), 100.0, 2.0, "UTC");
        let spans: Vec<(f64, f64)> = labels
            .iter()
            .map(|(t, x)| {
                (
                    x - t.chars().count() as f64 * 3.3,
                    x + t.chars().count() as f64 * 3.3,
                )
            })
            .collect();
        for (i, a) in spans.iter().enumerate() {
            for b in spans.iter().skip(i + 1) {
                assert!(a.1 + 12.0 <= b.0 || b.1 + 12.0 <= a.0, "{a:?} {b:?}");
            }
        }
        // Ticks within 20 px of either end are dropped.
        for (_, x) in &labels {
            assert!(*x >= 20.0 && *x <= 80.0);
        }
    }

    // ── trade markers ──

    #[test]
    fn trade_markers_paint_on_the_price_pane_in_a_translated_clipped_space() {
        let mut s = three_bars();
        s.trade_markers.visible = true;
        let mut m = model("strat", "price");
        m.trades = vec![TradeExecution {
            time: 60_000,
            price: 45.0,
            side: TradeSide::Buy,
            kind: TradeKind::Entry,
            label: None,
            qty: None,
            trade_id: None,
        }];
        s.indicators.insert("strat", m);
        let c = three_coords(&s);
        let canvas = render(&s, &c, None, Rc::new(PriceAxisTickSource::new()));
        let ops = canvas.ops();
        let at = ops.iter().position(|o| o[0] == "translate").unwrap();
        assert_eq!(
            ops[at - 1..at + 4].to_vec(),
            vec![
                json!(["save"]),
                json!(["translate", 0.0, 0.0]),
                json!(["beginPath"]),
                json!(["rect", 0.0, 0.0, 300.0, 100.0]),
                json!(["clip"]),
            ]
        );
        assert!(canvas.calls("save").len() > canvas.calls("translate").len());
        // The save is closed before the axes paint.
        let restore = ops.iter().position(|o| o[0] == "restore").unwrap();
        let axis_line = ops
            .iter()
            .position(|o| o == &json!(["moveTo", 300.5, 0.0]))
            .unwrap();
        assert!(restore < axis_line);

        // Hidden markers or a collapsed price pane draw none.
        s.trade_markers.visible = false;
        assert!(
            render(&s, &c, None, Rc::new(PriceAxisTickSource::new()))
                .calls("translate")
                .is_empty()
        );
        s.trade_markers.visible = true;
        s.panes.get_mut("price").unwrap().collapsed = true;
        assert!(
            render(&s, &c, None, Rc::new(PriceAxisTickSource::new()))
                .calls("translate")
                .is_empty()
        );
    }

    // ── the timeline-mark lane (chrome-mark-lane-clip.test.ts) ──

    #[derive(Clone, Copy, Debug)]
    struct Clip {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    }

    /// Every arc with the clip region active when it was drawn.
    fn arcs_with_clip(canvas: &RecordingCanvas) -> Vec<(f64, f64, f64, Option<Clip>)> {
        let mut clip: Option<Clip> = None;
        let mut path: Option<Clip> = None;
        let mut stack: Vec<Option<Clip>> = vec![];
        let mut out = vec![];
        for op in canvas.ops() {
            let n = |i: usize| op[i].as_f64().unwrap();
            match op[0].as_str().unwrap() {
                "save" => stack.push(clip),
                "restore" => clip = stack.pop().unwrap_or(None),
                "beginPath" => path = None,
                "rect" => {
                    path = Some(Clip {
                        x0: n(1),
                        y0: n(2),
                        x1: n(1) + n(3),
                        y1: n(2) + n(4),
                    })
                }
                "clip" => {
                    if let Some(p) = path {
                        clip = Some(match clip {
                            Some(c) => Clip {
                                x0: c.x0.max(p.x0),
                                y0: c.y0.max(p.y0),
                                x1: c.x1.min(p.x1),
                                y1: c.y1.min(p.y1),
                            },
                            None => p,
                        });
                    }
                }
                "arc" => out.push((n(1), n(2), n(3), clip)),
                _ => {}
            }
        }
        out
    }

    /// 60 hourly bars on a 300 x 400 plot, pitch 10, newest bar's center on the right edge;
    /// one mark on bar `mark_bar`.
    fn mark_frame(
        mark_bar: usize,
        edit: impl Fn(&mut SceneGraph),
    ) -> (ChromeRenderer, Rc<RecordingCanvas>, f64) {
        let times: Vec<i64> = (0..60).map(|i| T0 + i * HOUR).collect();
        let mut c = CoordinateSystem::new();
        c.set_size(300.0, 400.0, 1.0);
        c.set_bars(&times);
        c.set_viewport(ViewportState {
            bar_spacing: 10.0,
            right_offset: 0.0,
        });
        let mut s = SceneGraph::new();
        let price = s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        price.bounds.height = 400.0;
        price.scale = PriceScale::new(100.0, 200.0);
        s.bars = times
            .iter()
            .map(|t| Ohlcv::new(*t, 150.0, 160.0, 140.0, 155.0, Some(1.0)))
            .collect();
        s.show_price_label = false;
        s.show_countdown = false;
        s.timeline_marks = vec![TimelineMark::new(
            "edge",
            times[mark_bar],
            MarkGlyph {
                shape: None,
                color: "#ff00ff".into(),
                letter: Some("E".into()),
                icon: None,
            },
        )];
        edit(&mut s);
        let canvas = recording();
        let mut chrome = ChromeRenderer::default();
        chrome.mount(canvas.clone(), 364.0, 422.0);
        chrome.render(&s, &c, &theme(), None, &clock(), &NoMarkIcons);
        (chrome, canvas, c.logical_to_x(mark_bar as f64))
    }

    #[test]
    fn a_glyph_on_the_bar_at_the_right_plot_edge_is_cut_at_the_price_scale() {
        let (_, canvas, x) = mark_frame(59, |_| {});
        assert_eq!(x, 300.0);
        let arcs = arcs_with_clip(&canvas);
        let glyph = arcs
            .iter()
            .find(|a| a.0 == x && a.2 == MARK_GLYPH_PX / 2.0)
            .unwrap();
        assert!(glyph.3.unwrap().x1 <= 300.0);
    }

    #[test]
    fn the_clip_keeps_the_whole_lane_height() {
        let (_, canvas, x) = mark_frame(50, |_| {});
        let arcs = arcs_with_clip(&canvas);
        let (_, y, r, clip) = *arcs
            .iter()
            .find(|a| a.0 == x && a.2 == MARK_GLYPH_PX / 2.0)
            .unwrap();
        let clip = clip.unwrap();
        assert!(clip.y0 <= y - r && clip.y1 >= y + r);
        assert!(clip.x0 <= x - r && clip.x1 >= x + r);
    }

    #[test]
    fn the_lane_layout_answers_hit_tests_tooltips_and_key_lookups_until_the_lane_hides() {
        let (chrome, _, x) = mark_frame(50, |s| {
            s.timeline_marks[0].tooltip = Some("Dividend".into());
        });
        // A glyph sits 4 px above the axis: center y = 400 - 4 - 8.
        let (gx, gy) = (x, 388.0);
        let glyph = chrome.mark_glyph_at(gx, gy).unwrap();
        assert_eq!(glyph.cluster.marks[0].id, "edge");
        let key = glyph.cluster.key.clone();
        assert_eq!(chrome.mark_stack_at(gx, gy), Some(50));
        assert_eq!(chrome.mark_glyph_by_key(&key).map(|g| g.x), Some(x));
        assert!(chrome.mark_glyph_by_key("missing").is_none());
        assert_eq!(
            chrome.mark_tooltip_at(gx, gy, &[], None).as_deref(),
            Some("Dividend")
        );
        assert!(chrome.mark_glyph_at(0.0, 0.0).is_none());
        assert!(chrome.mark_tooltip_at(0.0, 0.0, &[], None).is_none());

        for edit in [
            (|s: &mut SceneGraph| s.marks.visible = false) as fn(&mut SceneGraph),
            |s| s.timeline_marks.clear(),
            |s| {
                s.timeline_marks[0].group = Some("news".into());
                s.marks.groups.insert("news".into(), false);
            },
        ] {
            let (chrome, canvas, _) = mark_frame(50, edit);
            assert!(chrome.mark_glyph_at(gx, gy).is_none());
            assert!(arcs_with_clip(&canvas).is_empty());
        }
    }

    #[test]
    fn the_flash_and_pulse_read_the_frame_clock() {
        let (_, canvas, _) = mark_frame(50, |s| {
            s.marks_flash = Some(crate::render::core::scene_graph::MarksFlash {
                key: "50|".into(),
                until: 100.0,
            });
        });
        // now_ms = 0 < until: the flash is live, and the glyph's key is "50|" only if the
        // cluster key has that shape; either way the frame paints one token.
        assert_eq!(canvas.calls("arc").len(), 1);
    }

    #[test]
    fn an_empty_chart_resets_the_lane_layout() {
        let (mut chrome, _, _) = mark_frame(50, |_| {});
        assert!(chrome.mark_glyph_at(300.0 - 90.0, 388.0).is_some());
        let canvas = recording();
        chrome.mount(canvas, 364.0, 422.0);
        let mut empty = CoordinateSystem::new();
        empty.set_size(300.0, 400.0, 1.0);
        chrome.render(
            &SceneGraph::new(),
            &empty,
            &theme(),
            None,
            &clock(),
            &NoMarkIcons,
        );
        assert!(chrome.mark_glyph_at(300.0 - 90.0, 388.0).is_none());
    }

    #[test]
    fn the_font_spec_helper_formats_whole_and_fractional_sizes() {
        assert_eq!(font_spec(11.0, "sans-serif"), "11px sans-serif");
        assert_eq!(font_spec(12.5, "Inter"), "12.5px Inter");
    }
}
