//! Dumps what the canvas painters of the last port stage draw, as recorded canvas call
//! streams: the geometry backend (`Canvas2dBackend`), the backdrop (grid and bands), the
//! chrome (axes, price tag, countdown, separators, trade markers, timeline-mark lane), the
//! crosshair, the volume layer and the VPVR layer.
//!
//! Every scenario is first generated as a plain JSON SPEC (bars, viewport, panes, scene
//! flags, style patch, indicator models, layer inputs, clocks). The Rust scene is then
//! built FROM THAT SPEC by the decoders below, and the parity script builds the original
//! TypeScript scene from the very same spec through the original's public calls. Each
//! scenario prints its spec and the recorded `ops` of every painter it ran, so the script
//! can replay it (see the README).
//!
//! Usage: `cargo run --release --example dump_series [seed] > series.json`

use std::rc::Rc;

use serde_json::{Map, Value, json};
use vela_core::chart_types::BasePainting;
use vela_core::format::{FormatSettings, HourCycleSetting, builtin_formatters};
use vela_core::indicators::volume::volume_layer_data;
use vela_core::indicators::vpvr::vpvr_layer_data;
use vela_core::intl::default_intl;
use vela_core::marks::{MarkGlyph, MarkGroup, MarkShape, TimelineMark};
use vela_core::model::volume_layers::{VolumeLayerData, VpvrLayerData};
use vela_core::model::{
    Background, BarColor, CandleBarColor, CandleKind, CandleStyle as SeriesCandleStyle, Fill,
    FillGradientStop, IndicatorModel, InputValue, Inputs, LineLikeKind, LineLikeStyle, LineStyle,
    Ohlcv, PaneAxisBand, PaneHint, PaneKind, PriceLine, SeriesData, SeriesDisplay, SeriesPoint,
    SeriesSpec, TradeExecution, TradeKind, TradeSide,
};
use vela_core::render::backdrop::BackdropRenderer;
use vela_core::render::backend::canvas2d_backend::Canvas2dBackend;
use vela_core::render::backend::render_backend::RenderBackend;
use vela_core::render::backend::vpvr_profile::{VpvrProfile, VpvrRow};
use vela_core::render::canvas::{CanvasImage, RecordingCanvas};
use vela_core::render::chrome::chrome_renderer::{AxisSurface, ChromeRenderer, FrameClock};
use vela_core::render::chrome::crosshair_renderer::{CrosshairRenderer, ExternalCrosshair};
use vela_core::render::chrome::marks_layout::{MarkLaneInput, layout_mark_lane, snap_mark_bar};
use vela_core::render::chrome::marks_paint::{
    MarkIcons, MarkPaintDeps, NoMarkIcons, paint_mark_lane,
};
use vela_core::render::chrome::price_axis_ticks::PriceAxisTickSource;
use vela_core::render::core::chart_config::{CandlePaintOverride, ChartStyle, CrosshairOverride};
use vela_core::render::core::scene_graph::{DrawingSlice, PaneAxisFormat, ScaleMode};
use vela_core::render::core::{
    CoordinateSystem, PaneBounds, PriceScale, SceneGraph, ViewportState,
};
use vela_core::render::shared::trade_markers::TradeMarkersState;
use vela_core::render::volume::{
    VolumeColors, VolumeGeom, VolumeRenderArgs, VolumeRenderer, paint_volume,
};
use vela_core::render::vpvr::{VpvrGeom, VpvrPaintStyle, VpvrRenderArgs, VpvrRenderer, paint_vpvr};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

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
    /// A value on a 1/1000 grid in `0..1`.
    fn unit(&mut self) -> f64 {
        self.below(1000) as f64 / 1000.0
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        r4(lo + (hi - lo) * self.unit())
    }
}

/// Rounds to four decimals so the spec JSON stays short (the rounded value is what both sides use).
fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

// ───────────────────────────── spec decoding helpers ─────────────────────────────

fn num(v: &Value, k: &str) -> f64 {
    v.get(k)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("spec number {k}"))
}
fn num_or(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn opt_num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}
fn string(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("spec string {k}"))
        .to_string()
}
fn opt_string(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}
fn flag(v: &Value, k: &str, d: bool) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn opt_flag(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}
fn items<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn line_style(s: &str) -> LineStyle {
    match s {
        "dashed" => LineStyle::Dashed,
        "dotted" => LineStyle::Dotted,
        _ => LineStyle::Solid,
    }
}

fn bar_from_json(v: &Value) -> Ohlcv {
    let a = v.as_array().expect("bar array");
    let f = |i: usize| a[i].as_f64().unwrap_or(f64::NAN);
    Ohlcv::new(
        a[0].as_i64().unwrap(),
        f(1),
        f(2),
        f(3),
        f(4),
        a.get(5).and_then(Value::as_f64),
    )
}

fn scale_from_json(v: &Value) -> PriceScale {
    PriceScale {
        min: num(v, "min"),
        max: num(v, "max"),
        log: opt_flag(v, "log"),
        invert: opt_flag(v, "invert"),
    }
}

// ───────────────────────────── style patch ─────────────────────────────

/// Writes `patch[key]` into `field` when the key is present (null clears an optional).
fn set_opt(field: &mut Option<String>, patch: &Value, key: &str) {
    if let Some(v) = patch.get(key) {
        *field = v.as_str().map(str::to_string);
    }
}
fn set_num(field: &mut f64, patch: &Value, key: &str) {
    if let Some(n) = patch.get(key).and_then(Value::as_f64) {
        *field = n;
    }
}
fn set_flag(field: &mut bool, patch: &Value, key: &str) {
    if let Some(b) = patch.get(key).and_then(Value::as_bool) {
        *field = b;
    }
}

fn apply_style(style: &mut ChartStyle, p: &Value) {
    set_num(&mut style.font_size, p, "fontSize");
    set_opt(&mut style.border_color, p, "borderColor");
    set_opt(&mut style.separator_color, p, "separatorColor");
    if let Some(g) = p.get("gridVert") {
        set_flag(&mut style.grid_vert.visible, g, "visible");
        set_opt(&mut style.grid_vert.color, g, "color");
    }
    if let Some(g) = p.get("gridHorz") {
        set_flag(&mut style.grid_horz.visible, g, "visible");
        set_opt(&mut style.grid_horz.color, g, "color");
    }
    if let Some(c) = p.get("crosshair") {
        set_opt(&mut style.crosshair.color, c, "color");
        set_num(&mut style.crosshair.width, c, "width");
        if let Some(s) = c.get("style").and_then(Value::as_str) {
            style.crosshair.style = line_style(s);
        }
        set_num(&mut style.crosshair.opacity, c, "opacity");
        set_opt(&mut style.crosshair.label_background, c, "labelBackground");
    }
    if let Some(c) = p.get("candle") {
        set_flag(&mut style.candle.body_visible, c, "bodyVisible");
        set_flag(&mut style.candle.border_visible, c, "borderVisible");
        set_opt(&mut style.candle.border_up_color, c, "borderUpColor");
        set_opt(&mut style.candle.border_down_color, c, "borderDownColor");
        set_flag(&mut style.candle.wick_visible, c, "wickVisible");
        set_opt(&mut style.candle.wick_up_color, c, "wickUpColor");
        set_opt(&mut style.candle.wick_down_color, c, "wickDownColor");
    }
    if let Some(c) = p.get("bars") {
        set_opt(&mut style.bars.up_color, c, "upColor");
        set_opt(&mut style.bars.down_color, c, "downColor");
    }
    if let Some(c) = p.get("line") {
        set_opt(&mut style.line.color, c, "color");
        set_num(&mut style.line.width, c, "width");
    }
    if let Some(c) = p.get("area") {
        set_opt(&mut style.area.line_color, c, "lineColor");
        set_num(&mut style.area.width, c, "width");
        set_opt(&mut style.area.top_color, c, "topColor");
        set_opt(&mut style.area.bottom_color, c, "bottomColor");
    }
    if let Some(c) = p.get("baseline") {
        set_opt(&mut style.baseline.top_line_color, c, "topLineColor");
        set_opt(&mut style.baseline.bottom_line_color, c, "bottomLineColor");
        set_opt(&mut style.baseline.top_fill_color, c, "topFillColor");
        set_opt(&mut style.baseline.top_fill_color2, c, "topFillColor2");
        set_opt(&mut style.baseline.bottom_fill_color, c, "bottomFillColor");
        set_opt(
            &mut style.baseline.bottom_fill_color2,
            c,
            "bottomFillColor2",
        );
        set_num(&mut style.baseline.width, c, "width");
        set_num(&mut style.baseline.baseline_level, c, "baselineLevel");
    }
    if let Some(c) = p.get("columns") {
        set_opt(&mut style.columns.up_color, c, "upColor");
        set_opt(&mut style.columns.down_color, c, "downColor");
    }
    if let Some(c) = p.get("sessions") {
        if let Some(s) = opt_string(c, "premarketColor") {
            style.sessions.premarket_color = s;
        }
        if let Some(s) = opt_string(c, "postmarketColor") {
            style.sessions.postmarket_color = s;
        }
        if let Some(s) = opt_string(c, "extendedColor") {
            style.sessions.extended_color = s;
        }
    }
}

// ───────────────────────────── model decoding ─────────────────────────────

fn point_from_json(v: &Value) -> SeriesPoint {
    SeriesPoint {
        time: v["time"].as_i64().unwrap(),
        value: v.get("value").and_then(Value::as_f64),
        color: opt_string(v, "color"),
    }
}

fn series_from_json(v: &Value, pane_id: &str) -> SeriesSpec {
    let kind = string(v, "kind");
    let data = match kind.as_str() {
        "candle" | "bar" => SeriesData::Candle {
            kind: if kind == "bar" {
                CandleKind::Bar
            } else {
                CandleKind::Candle
            },
            bars: items(v, "bars").iter().map(bar_from_json).collect(),
            style: v.get("style").map(|s| SeriesCandleStyle {
                up: opt_string(s, "up"),
                down: opt_string(s, "down"),
                ..Default::default()
            }),
            bar_colors: v.get("barColors").and_then(Value::as_array).map(|a| {
                a.iter()
                    .map(|c| {
                        (!c.is_null()).then(|| CandleBarColor {
                            color: opt_string(c, "color"),
                            wick_color: opt_string(c, "wickColor"),
                            border_color: opt_string(c, "borderColor"),
                        })
                    })
                    .collect()
            }),
        },
        "markers" => SeriesData::Markers { markers: vec![] },
        _ => {
            let k = match kind.as_str() {
                "area" => LineLikeKind::Area,
                "step" => LineLikeKind::Step,
                "histogram" => LineLikeKind::Histogram,
                "columns" => LineLikeKind::Columns,
                "circles" => LineLikeKind::Circles,
                "cross" => LineLikeKind::Cross,
                _ => LineLikeKind::Line,
            };
            let st = &v["style"];
            SeriesData::LineLike {
                kind: k,
                points: items(v, "points").iter().map(point_from_json).collect(),
                style: LineLikeStyle {
                    color: string(st, "color"),
                    width: num(st, "width"),
                    line_style: line_style(&string(st, "lineStyle")),
                    base: opt_num(st, "base"),
                },
            }
        }
    };
    SeriesSpec {
        id: string(v, "id"),
        title: string(v, "id"),
        display_title: None,
        pane_id: pane_id.to_string(),
        z_order: None,
        visible: opt_flag(v, "visible"),
        display: v.get("display").map(|d| SeriesDisplay {
            pane: opt_flag(d, "pane"),
            price_scale: opt_flag(d, "priceScale"),
            legend: opt_flag(d, "legend"),
            data_window: opt_flag(d, "dataWindow"),
        }),
        overlay: flag(v, "overlay", false),
        data,
    }
}

fn fill_from_json(v: &Value, pane_id: &str) -> Fill {
    Fill {
        id: string(v, "id"),
        pane_id: pane_id.to_string(),
        from_series_id: string(v, "fromSeriesId"),
        to_series_id: string(v, "toSeriesId"),
        color: opt_string(v, "color"),
        colors: v
            .get("colors")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|c| c.as_str().map(str::to_string)).collect()),
        gradient: v.get("gradient").and_then(Value::as_array).map(|a| {
            a.iter()
                .map(|g| {
                    (!g.is_null()).then(|| FillGradientStop {
                        top_value: num(g, "topValue"),
                        bottom_value: num(g, "bottomValue"),
                        top_color: string(g, "topColor"),
                        bottom_color: string(g, "bottomColor"),
                    })
                })
                .collect()
        }),
        overlay: flag(v, "overlay", false),
    }
}

fn model_from_json(v: &Value) -> IndicatorModel {
    let pane_id = string(v, "paneId");
    IndicatorModel {
        id: string(v, "id"),
        title: string(v, "id"),
        shorttitle: None,
        display_title: None,
        display_shorttitle: None,
        overlay: true,
        pane_hint: PaneHint::Price,
        native: None,
        legend: None,
        pane_axis: None,
        pane_id: Some(pane_id.clone()),
        own_scale: flag(v, "ownScale", false),
        anchor_time: None,
        series: items(v, "series")
            .iter()
            .map(|s| series_from_json(s, &pane_id))
            .collect(),
        fills: items(v, "fills")
            .iter()
            .map(|f| fill_from_json(f, &pane_id))
            .collect(),
        backgrounds: items(v, "backgrounds")
            .iter()
            .map(|b| Background {
                id: string(b, "id"),
                pane_id: pane_id.clone(),
                from: b["from"].as_i64().unwrap(),
                to: b["to"].as_i64().unwrap(),
                color: string(b, "color"),
                overlay: flag(b, "overlay", false),
            })
            .collect(),
        price_lines: items(v, "priceLines")
            .iter()
            .map(|p| PriceLine {
                id: string(p, "id"),
                pane_id: pane_id.clone(),
                price: num(p, "price"),
                color: opt_string(p, "color"),
                line_style: opt_string(p, "lineStyle").map(|s| line_style(&s)),
                width: opt_num(p, "width"),
                title: None,
                display_title: None,
            })
            .collect(),
        lines: vec![],
        boxes: vec![],
        labels: vec![],
        polylines: vec![],
        linefills: vec![],
        tables: vec![],
        bar_colors: items(v, "barColors")
            .iter()
            .map(|b| BarColor {
                time: b["time"].as_i64().unwrap(),
                color: string(b, "color"),
            })
            .collect(),
        trades: items(v, "trades")
            .iter()
            .map(|t| TradeExecution {
                time: t["time"].as_i64().unwrap(),
                price: num(t, "price"),
                side: if string(t, "side") == "buy" {
                    TradeSide::Buy
                } else {
                    TradeSide::Sell
                },
                kind: if string(t, "kind") == "entry" {
                    TradeKind::Entry
                } else {
                    TradeKind::Exit
                },
                label: opt_string(t, "label"),
                qty: opt_num(t, "qty"),
                trade_id: opt_string(t, "tradeId"),
            })
            .collect(),
        inputs: vec![],
        input_values: Default::default(),
        props: vec![],
        prop_values: Default::default(),
    }
}

// ───────────────────────────── scene decoding ─────────────────────────────

fn mark_from_json(v: &Value) -> TimelineMark {
    let g = &v["glyph"];
    let shape = opt_string(g, "shape").map(|s| match s.as_str() {
        "square" => MarkShape::Square,
        "diamond" => MarkShape::Diamond,
        "pin" => MarkShape::Pin,
        _ => MarkShape::Circle,
    });
    let mut m = TimelineMark::new(
        string(v, "id"),
        v["time"].as_i64().unwrap(),
        MarkGlyph {
            shape,
            color: string(g, "color"),
            letter: opt_string(g, "letter"),
            icon: opt_string(g, "icon"),
        },
    );
    m.group = opt_string(v, "group");
    m.tooltip = opt_string(v, "tooltip");
    m
}

fn group_from_json(v: &Value) -> MarkGroup {
    MarkGroup {
        id: string(v, "id"),
        label: string(v, "label"),
        visible: opt_flag(v, "visible"),
        parent: opt_string(v, "parent"),
    }
}

struct Frame {
    scene: SceneGraph,
    coords: CoordinateSystem,
    theme: Value,
    full_w: f64,
    full_h: f64,
    dpr: f64,
}

fn build_frame(spec: &Value, datasets: &[Vec<Ohlcv>], themes: &[Value]) -> Frame {
    let bars = datasets[spec["dataset"].as_u64().unwrap() as usize].clone();
    let mut scene = SceneGraph::new();
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    scene.bars = bars;

    let s = &spec["scene"];
    if let Some(p) = opt_string(s, "priceStyle") {
        scene.price_style = p;
    }
    if opt_string(s, "basePainting").as_deref() == Some("none") {
        scene.base_painting = BasePainting::None;
    }
    scene.baseline_value = opt_num(s, "baselineValue");
    scene.show_price_line = flag(s, "showPriceLine", true);
    scene.show_price_label = flag(s, "showPriceLabel", true);
    scene.show_countdown = flag(s, "showCountdown", true);
    scene.log_scale = flag(s, "logScale", false);
    scene.invert_scale = flag(s, "invertScale", false);
    scene.price_mintick = opt_num(s, "priceMintick");
    if let Some(m) = opt_string(s, "scaleMode").and_then(|m| ScaleMode::parse(&m)) {
        scene.scale_mode = m;
    }
    scene.percent_baseline = num_or(s, "percentBaseline", 0.0);
    if let Some(z) = opt_string(s, "timezone") {
        scene.timezone = z;
    }
    if let Some(fm) = s.get("format") {
        let cycle = match opt_string(fm, "hourCycle").as_deref() {
            Some("h12") => HourCycleSetting::H12,
            Some("locale") => HourCycleSetting::Locale,
            _ => HourCycleSetting::H23,
        };
        scene.format = Some(builtin_formatters(FormatSettings::new(
            string(fm, "locale"),
            string(fm, "numberingSystem"),
            cycle,
            flag(fm, "english", true),
            default_intl(),
        )));
    }
    scene.show_grid = flag(s, "showGrid", true);
    scene.show_axis_labels = flag(s, "showAxisLabels", true);
    scene.candle_z = num_or(s, "candleZ", 0.0);
    scene.candles_hidden = flag(s, "candlesHidden", false);
    if let Some(st) = s.get("style") {
        apply_style(&mut scene.style, st);
    }
    if let Some(o) = s.get("candleOverride") {
        scene.candle_override = Some(CandlePaintOverride {
            up_color: opt_string(o, "upColor"),
            down_color: opt_string(o, "downColor"),
            body_visible: opt_flag(o, "bodyVisible"),
            border_visible: opt_flag(o, "borderVisible"),
            border_up_color: opt_string(o, "borderUpColor"),
            border_down_color: opt_string(o, "borderDownColor"),
            wick_visible: opt_flag(o, "wickVisible"),
            wick_up_color: opt_string(o, "wickUpColor"),
            wick_down_color: opt_string(o, "wickDownColor"),
        });
    }
    if let Some(c) = s.get("crosshair").filter(|c| !c.is_null()) {
        scene.crosshair = Some(vela_core::render::core::scene_graph::CrosshairPoint {
            x: num(c, "x"),
            y: num(c, "y"),
        });
    }
    if let Some(o) = s.get("crosshairOverride").filter(|c| !c.is_null()) {
        scene.crosshair_override =
            Some(serde_json::from_value::<CrosshairOverride>(o.clone()).unwrap());
    }
    for h in items(s, "highlights") {
        scene
            .highlights
            .push(vela_core::render::core::scene_graph::HighlightArea {
                from: h["from"].as_i64().unwrap(),
                to: h["to"].as_i64().unwrap(),
                color: string(h, "color"),
            });
    }
    if let Some(z) = s.get("sessionZones").filter(|z| !z.is_null()) {
        let pairs = |k: &str| -> Vec<(i64, i64)> {
            items(z, k)
                .iter()
                .map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap()))
                .collect()
        };
        scene.session_zones = Some(vela_core::render::core::scene_graph::SessionZones {
            pre: pairs("pre"),
            post: pairs("post"),
            extended: pairs("extended"),
        });
    }
    if let Some(t) = s.get("tradeMarkers") {
        scene.trade_markers = TradeMarkersState {
            visible: flag(t, "visible", true),
            labels: flag(t, "labels", true),
            qty: flag(t, "qty", true),
            ..Default::default()
        };
    }
    if let Some(m) = s.get("marks") {
        scene.marks.visible = flag(m, "visible", true);
        if let Some(g) = m.get("groups").and_then(Value::as_object) {
            for (k, v) in g {
                scene.marks.groups.insert(k.clone(), v.as_bool().unwrap());
            }
        }
    }
    scene.timeline_marks = items(s, "timelineMarks")
        .iter()
        .map(mark_from_json)
        .collect();
    scene.mark_groups = items(s, "markGroups").iter().map(group_from_json).collect();
    scene.marks_expanded_stack = s.get("marksExpandedStack").and_then(Value::as_i64);
    scene.marks_hover_key = opt_string(s, "marksHoverKey");
    scene.marks_hover_since = num_or(s, "marksHoverSince", 0.0);
    scene.marks_active_key = opt_string(s, "marksActiveKey");
    if let Some(f) = s.get("marksFlash").filter(|f| !f.is_null()) {
        scene.marks_flash = Some(vela_core::render::core::scene_graph::MarksFlash {
            key: string(f, "key"),
            until: num(f, "until"),
        });
    }

    for p in items(spec, "panes") {
        let kind = if string(p, "kind") == "price" {
            PaneKind::Price
        } else {
            PaneKind::Study
        };
        let pane = scene.ensure_pane(
            &string(p, "id"),
            kind,
            p["order"].as_i64().unwrap() as i32,
            opt_num(p, "heightWeight"),
        );
        pane.bounds = PaneBounds {
            top: num(p, "top"),
            height: num(p, "height"),
        };
        pane.scale = scale_from_json(&p["scale"]);
        pane.scale_target = pane.scale;
        pane.initialized = true;
        pane.collapsed = flag(p, "collapsed", false);
        pane.axis_format = opt_string(p, "axisFormat").map(|f| {
            if f == "volume" {
                PaneAxisFormat::Volume
            } else {
                PaneAxisFormat::NoAxis
            }
        });
        pane.axis_bands = p.get("axisBands").and_then(Value::as_array).map(|a| {
            a.iter()
                .map(|b| PaneAxisBand {
                    frac: num(b, "frac"),
                    label: string(b, "label"),
                })
                .collect()
        });
        pane.scale_mode = opt_string(p, "scaleMode").and_then(|m| ScaleMode::parse(&m));
        pane.log_scale = opt_flag(p, "logScale");
        pane.invert = opt_flag(p, "invert");
        pane.percent_baseline = num_or(p, "percentBaseline", 0.0);
    }
    for m in items(spec, "indicators") {
        let model = model_from_json(m);
        let id = model.id.clone();
        scene.indicators.insert(id.clone(), model);
        match opt_num(m, "z") {
            Some(z) => scene.set_indicator_z(&id, z),
            None => scene.assign_indicator_z(&id),
        }
        if let Some(off) = m.get("anchorOffset").and_then(Value::as_i64) {
            scene.set_anchor_offset(&id, off);
        }
        if let Some(sc) = m.get("ownScaleWindow") {
            let slot = scene.ensure_indicator_scale(&id, None);
            slot.scale = scale_from_json(sc);
            slot.scale_target = slot.scale;
            slot.initialized = true;
        }
    }
    for (pane_id, list) in spec
        .get("slices")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let slices: Vec<DrawingSlice> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|z| DrawingSlice {
                before_z: z.as_f64().unwrap_or(f64::INFINITY),
                canvas: Rc::new(()) as CanvasImage,
            })
            .collect();
        scene.drawing_slices.insert(pane_id.clone(), slices);
    }

    let width = num(spec, "width");
    let height = num(spec, "height");
    let dpr = num(spec, "dpr");
    let mut coords = CoordinateSystem::new();
    coords.set_size(width, height, dpr);
    coords.set_bars(&times);
    coords.set_viewport(ViewportState {
        bar_spacing: num(&spec["viewport"], "barSpacing"),
        right_offset: num(&spec["viewport"], "rightOffset"),
    });
    if let Some(p) = opt_num(spec, "pitchScale") {
        coords.set_pitch_scale(p);
    }
    Frame {
        scene,
        coords,
        theme: themes[spec["theme"].as_u64().unwrap() as usize].clone(),
        full_w: num(spec, "fullWidth"),
        full_h: num(spec, "fullHeight"),
        dpr,
    }
}

fn recording() -> Rc<RecordingCanvas> {
    Rc::new(RecordingCanvas::new())
}

// ───────────────────────────── dataset generation ─────────────────────────────

struct DatasetSpec {
    name: &'static str,
    n: usize,
    interval: i64,
    start: i64,
    /// Price unit (one tick) and the opening price in ticks.
    tick: f64,
    open_ticks: i64,
    step: i64,
    gaps: Gaps,
    quirks: bool,
    signed: bool,
    nan: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Gaps {
    None,
    /// A session break (skip 16 bars' worth) every 78 bars.
    Sessions,
    /// Weekends skipped (daily bars).
    Weekends,
}

fn make_dataset(rng: &mut Rng, d: &DatasetSpec) -> Vec<Value> {
    let mut out = Vec::new();
    let mut time = d.start;
    let mut close = d.open_ticks;
    for i in 0..d.n {
        let open = close;
        let floor = if d.signed { i64::MIN } else { 1 };
        close = (close + rng.below((2 * d.step + 1) as u64) as i64 - d.step).max(floor);
        let hi = open.max(close) + rng.below((d.step + 1) as u64) as i64;
        let lo = (open.min(close) - rng.below((d.step + 1) as u64) as i64).max(floor);
        let mut volume = Some(100.0 + rng.below(900) as f64);
        let (mut o, mut h, mut l, mut c) = (open, hi, lo, close);
        if d.quirks {
            if rng.chance(8) {
                // A zero-range bar.
                h = open;
                l = open;
                c = open;
                close = open;
            }
            if rng.chance(6) {
                volume = None;
            }
            if rng.chance(4) {
                volume = Some(0.0);
            }
            if rng.chance(3) {
                volume = Some(5_000_000.0);
            }
            if rng.chance(5) {
                // An open above its high would be invalid data: keep open inside the range.
                o = o.clamp(l, h);
            }
        }
        if d.nan && rng.chance(4) {
            // A bar with no usable prices (JSON writes NaN as null; both sides read it back as NaN).
            out.push(json!([time, null, null, null, null, volume]));
            time += d.interval;
            continue;
        }
        out.push(json!([
            time,
            o as f64 * d.tick,
            h as f64 * d.tick,
            l as f64 * d.tick,
            c as f64 * d.tick,
            volume
        ]));
        time += d.interval;
        match d.gaps {
            Gaps::None => {}
            Gaps::Sessions => {
                if (i + 1) % 78 == 0 {
                    time += 16 * HOUR;
                }
            }
            Gaps::Weekends => {
                // Daily bars: Saturday and Sunday are skipped (2023-01-02 is a Monday).
                if (i + 1) % 5 == 0 {
                    time += 2 * DAY;
                }
            }
        }
    }
    out
}

fn datasets(rng: &mut Rng) -> Vec<Value> {
    let specs = [
        DatasetSpec {
            name: "min1",
            n: 300,
            interval: MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 10_000,
            step: 40,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "m5-sessions",
            n: 320,
            interval: 5 * MINUTE,
            start: 1_699_977_600_000,
            tick: 0.01,
            open_ticks: 18_550,
            step: 60,
            gaps: Gaps::Sessions,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "daily-weekends",
            n: 260,
            interval: DAY,
            start: 1_672_617_600_000,
            tick: 0.01,
            open_ticks: 15_000,
            step: 250,
            gaps: Gaps::Weekends,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "tiny-prices",
            n: 300,
            interval: MINUTE,
            start: 1_699_920_000_000,
            tick: 0.0000001,
            open_ticks: 1_230_000,
            step: 6_000,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "big-prices",
            n: 280,
            interval: HOUR,
            start: 1_699_920_000_000,
            tick: 0.5,
            open_ticks: 90_000,
            step: 400,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "quirks",
            n: 200,
            interval: 5 * MINUTE,
            start: 1_699_920_000_000,
            tick: 0.25,
            open_ticks: 400,
            step: 6,
            gaps: Gaps::None,
            quirks: true,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "seconds",
            n: 300,
            interval: 1000,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 4_250,
            step: 4,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "dense",
            n: 5000,
            interval: MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 50_000,
            step: 30,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "three-bars",
            n: 3,
            interval: 15 * MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 7_000,
            step: 50,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "weekly",
            n: 150,
            interval: 7 * DAY,
            start: 1_672_617_600_000,
            tick: 0.01,
            open_ticks: 22_000,
            step: 600,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "empty",
            n: 0,
            interval: MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 100,
            step: 5,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "single-bar",
            n: 1,
            interval: MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 12_345,
            step: 5,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "uneven",
            n: 250,
            interval: 3 * MINUTE,
            start: 1_699_920_000_000,
            tick: 0.05,
            open_ticks: 2_000,
            step: 20,
            gaps: Gaps::Sessions,
            quirks: true,
            signed: false,
            nan: false,
        },
        DatasetSpec {
            name: "nan-bars",
            n: 220,
            interval: 5 * MINUTE,
            start: 1_699_920_000_000,
            tick: 0.01,
            open_ticks: 9_000,
            step: 30,
            gaps: Gaps::None,
            quirks: false,
            signed: false,
            nan: true,
        },
        DatasetSpec {
            name: "signed",
            n: 240,
            interval: HOUR,
            start: 1_699_920_000_000,
            tick: 0.5,
            open_ticks: 20,
            step: 12,
            gaps: Gaps::None,
            quirks: false,
            signed: true,
            nan: false,
        },
    ];
    specs
        .iter()
        .map(|d| json!({ "name": d.name, "interval": d.interval, "bars": make_dataset(rng, d) }))
        .collect()
}

const DS_MIN1: usize = 0;
const DS_SESSIONS: usize = 1;
const DS_DAILY: usize = 2;
const DS_TINY: usize = 3;
const DS_BIG: usize = 4;
const DS_QUIRKS: usize = 5;
const DS_SECONDS: usize = 6;
const DS_DENSE: usize = 7;
const DS_THREE: usize = 8;
const DS_WEEKLY: usize = 9;
const DS_EMPTY: usize = 10;
const DS_SINGLE: usize = 11;
const DS_UNEVEN: usize = 12;
const DS_NAN: usize = 13;
const DS_SIGNED: usize = 14;

// ───────────────────────────── spec generation ─────────────────────────────

const THEME_COLORS: [&str; 6] = [
    "#2962ff", "#ff9800", "#e91e63", "#00bcd4", "#ab47bc", "#ffffff",
];
const FILL_COLORS: [&str; 5] = [
    "rgba(41, 98, 255, 0.25)",
    "#ff980040",
    "rgba(0,200,83,0.15)",
    "#e91e6333",
    "rgb(120, 123, 134)",
];
const STYLES: [&str; 3] = ["solid", "dashed", "dotted"];

fn themes() -> Vec<Value> {
    vec![
        json!({ "background": "#131722", "textColor": "#d1d4dc", "gridColor": "#2a2e39", "borderColor": "#363a45", "upColor": "#26a69a", "downColor": "#ef5350", "fontFamily": "Inter, sans-serif" }),
        json!({ "background": "#ffffff", "textColor": "#131722", "gridColor": "#e0e3eb", "borderColor": "#d1d4dc", "upColor": "#089981", "downColor": "#f23645", "fontFamily": "Roboto, Arial" }),
        json!({ "background": "#000000", "textColor": "#cccccc", "gridColor": "#222222", "borderColor": "#333333", "upColor": "#00AA00", "downColor": "#AA0000", "fontFamily": "sans-serif" }),
        json!({ "background": "#fafafa", "textColor": "#444", "gridColor": "rgba(0,0,0,0.06)", "borderColor": "#ccc", "upColor": "#fff", "downColor": "#000", "fontFamily": "monospace" }),
    ]
}

/// What a scenario family stresses; the generator spends its variety there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Backend,
    Backdrop,
    Chrome,
    Crosshair,
}

fn visible_window(n: usize, width: f64, spacing: f64, right_offset: f64) -> (f64, f64) {
    let right_edge = n as f64 - 1.0 + right_offset;
    (right_edge - width / spacing, right_edge)
}

fn price_range(bars: &[Value], from: f64, to: f64) -> (f64, f64) {
    let n = bars.len();
    let i0 = from.floor().max(0.0) as usize;
    let i1 = (to.ceil().min(n as f64 - 1.0)).max(0.0) as usize;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for b in bars.iter().take(i1 + 1).skip(i0) {
        lo = lo.min(b[3].as_f64().unwrap_or(lo));
        hi = hi.max(b[2].as_f64().unwrap_or(hi));
    }
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    if hi <= lo {
        return (lo - 1.0, hi + 1.0);
    }
    (lo, hi)
}

/// An index-aligned value series over `n` slots.
fn gen_values(
    rng: &mut Rng,
    bars: &[Value],
    kind: &str,
    around_price: bool,
    extra: usize,
    lead_gap: usize,
) -> Vec<Option<f64>> {
    let n = bars.len();
    let mut out = Vec::new();
    let mut wave = rng.unit() * 6.0;
    for i in 0..n + extra {
        wave += rng.unit() - 0.5;
        let v = if around_price {
            let close = bars
                .get(i)
                .or(bars.last())
                .map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0));
            close * (1.0 + 0.004 * wave)
        } else {
            match kind {
                "histogram" | "columns" => (wave * 10.0).sin() * 40.0 + 5.0,
                _ => 50.0 + wave * 8.0 + (i as f64 * 0.05).sin() * 10.0,
            }
        };
        if i < lead_gap || rng.chance(5) {
            out.push(None);
        } else {
            out.push(Some(r4(v)));
        }
    }
    out
}

fn bar_time_at(bars: &[Value], i: i64, interval: i64) -> i64 {
    if i >= 0 && (i as usize) < bars.len() {
        bars[i as usize][0].as_i64().unwrap()
    } else if i < 0 {
        bars.first().map_or(0, |b| b[0].as_i64().unwrap()) + i * interval
    } else {
        bars.last().map_or(0, |b| b[0].as_i64().unwrap()) + (i - bars.len() as i64 + 1) * interval
    }
}

struct ModelCfg {
    pane: String,
    around_price: bool,
}

fn gen_model(rng: &mut Rng, id: String, cfg: &ModelCfg, bars: &[Value], interval: i64) -> Value {
    let n = bars.len();
    let offset: i64 = match rng.below(6) {
        0 if n > 20 => rng.below(15) as i64 + 1,
        1 if n > 20 => -(rng.below(6) as i64 + 1),
        _ => 0,
    };
    // A series covers the slots from the model's anchor bar; a negative offset starts before bar 0.
    let first_slot = offset;
    let extra = if rng.chance(25) { rng.below(8) + 1 } else { 0 };
    let slots = (n as i64 + extra as i64 - first_slot).max(0) as usize;
    let mut series = Vec::new();
    let kinds = [
        "line",
        "area",
        "step",
        "histogram",
        "columns",
        "circles",
        "cross",
        "candle",
        "bar",
        "markers",
    ];
    let count = 1 + rng.below(3);
    for k in 0..count {
        let kind = kinds[rng.below(kinds.len() as u64)];
        let sid = format!("{id}-s{k}");
        let color = THEME_COLORS[rng.below(THEME_COLORS.len() as u64)];
        let mut sv = json!({ "id": sid, "kind": kind });
        if matches!(kind, "candle" | "bar") {
            let mut arr = Vec::new();
            let mut barcolors = Vec::new();
            let mut any = false;
            for j in 0..slots {
                let slot = j as i64 + first_slot;
                let t = bar_time_at(bars, slot, interval);
                if rng.chance(6) {
                    arr.push(json!([t, null, null, null, null]));
                    barcolors.push(Value::Null);
                    continue;
                }
                let base = bars
                    .get(slot.max(0) as usize)
                    .or(bars.last())
                    .map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0));
                let (o, c) = (
                    base * (1.0 + 0.002 * (rng.unit() - 0.5)),
                    base * (1.0 + 0.002 * (rng.unit() - 0.5)),
                );
                let (h, l) = (o.max(c) * 1.001, o.min(c) * 0.999);
                arr.push(json!([t, r4(o), r4(h), r4(l), r4(c)]));
                if rng.chance(15) {
                    any = true;
                    barcolors.push(json!({
                        "color": if rng.chance(70) { Some("#ffeb3b") } else { None },
                        "wickColor": if rng.chance(50) { Some("#9c27b0") } else { None },
                        "borderColor": if rng.chance(40) { Some("#ff5722") } else { None },
                    }));
                } else {
                    barcolors.push(Value::Null);
                }
            }
            sv["bars"] = Value::Array(arr);
            if any || rng.chance(30) {
                sv["barColors"] = Value::Array(barcolors);
            }
            if rng.chance(50) {
                sv["style"] = json!({ "up": "#4caf50", "down": "#f44336" });
            }
        } else if kind != "markers" {
            let lead = if rng.chance(40) { rng.below(20) } else { 0 };
            let values = gen_values(rng, bars, kind, cfg.around_price, extra, lead);
            let per_point = rng.chance(35);
            let points: Vec<Value> = (0..slots)
                .map(|j| {
                    let slot = j as i64 + first_slot;
                    let v = values.get(slot.max(0) as usize).copied().flatten();
                    let mut p = json!({ "time": bar_time_at(bars, slot, interval), "value": v });
                    if per_point && rng.chance(40) {
                        p["color"] = json!(THEME_COLORS[rng.below(THEME_COLORS.len() as u64)]);
                    }
                    p
                })
                .collect();
            sv["points"] = Value::Array(points);
            let mut style = json!({
                "color": color,
                "width": rng.pick(&[0.5, 1.0, 1.5, 2.0, 3.0]),
                "lineStyle": STYLES[rng.below(3)],
            });
            if matches!(kind, "histogram" | "columns" | "area") && rng.chance(60) {
                style["base"] = json!(if cfg.around_price {
                    r4(bars
                        .get(n / 2)
                        .map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0)))
                } else {
                    20.0
                });
            }
            sv["style"] = style;
        }
        match rng.below(12) {
            0 => sv["visible"] = json!(false),
            1 => sv["display"] = json!({ "pane": false }),
            2 => sv["display"] = json!({ "legend": false, "priceScale": false }),
            3 => sv["overlay"] = json!(true),
            _ => {}
        }
        series.push(sv);
    }
    // Fills between two line-like series of this model.
    let mut fills = Vec::new();
    let line_ids: Vec<String> = series
        .iter()
        .filter(|s| s.get("points").is_some())
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    if line_ids.len() >= 2 || (line_ids.len() == 1 && rng.chance(30)) {
        for f in 0..rng.below(3) {
            let from = line_ids[0].clone();
            let to = line_ids[line_ids.len() - 1 - rng.below(2).min(line_ids.len() - 1)].clone();
            let mut fill =
                json!({ "id": format!("{id}-f{f}"), "fromSeriesId": from, "toSeriesId": to });
            match rng.below(5) {
                0 => {
                    let colors: Vec<Value> = (0..slots.saturating_sub(rng.below(3)))
                        .map(|_| match rng.below(4) {
                            0 => Value::Null,
                            1 => json!(""),
                            _ => json!(FILL_COLORS[rng.below(FILL_COLORS.len() as u64)]),
                        })
                        .collect();
                    fill["colors"] = Value::Array(colors);
                }
                1 => {
                    let flat = rng.chance(30);
                    let g: Vec<Value> = (0..slots)
                        .map(|j| {
                            if rng.chance(10) {
                                return Value::Null;
                            }
                            let slot = (j as i64 + first_slot).max(0) as usize;
                            let base = bars.get(slot).or(bars.last()).map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0));
                            let (top, bot) = if flat { (base, base) } else { (base * 1.01, base * 0.99) };
                            // Runs of identical gradients exercise the original's gradient pool.
                            json!({ "topValue": r4(if j % 7 < 4 { top } else { top * 1.002 }), "bottomValue": r4(bot), "topColor": "rgba(38,166,154,0.5)", "bottomColor": if j % 11 < 6 { "rgba(38,166,154,0)" } else { "#ef535020" } })
                        })
                        .collect();
                    fill["gradient"] = Value::Array(g);
                }
                _ => fill["color"] = json!(FILL_COLORS[rng.below(FILL_COLORS.len() as u64)]),
            }
            if rng.chance(20) {
                fill["overlay"] = json!(true);
            }
            fills.push(fill);
        }
    }
    let mut model = json!({ "id": id, "paneId": cfg.pane, "series": series, "fills": fills });
    if offset != 0 {
        model["anchorOffset"] = json!(offset);
    }
    // Backgrounds over time spans around the visible bars.
    if n > 10 && rng.chance(40) {
        let mut bgs = Vec::new();
        for b in 0..1 + rng.below(3) {
            let a = rng.below(n as u64 - 1) as i64;
            let len = 1 + rng.below(30) as i64;
            let from =
                bar_time_at(bars, a, interval) - if rng.chance(30) { interval / 2 } else { 0 };
            let to = bar_time_at(bars, a + len, interval);
            let mut bg = json!({ "id": format!("{id}-b{b}"), "from": from, "to": if rng.chance(8) { from } else { to }, "color": FILL_COLORS[rng.below(FILL_COLORS.len() as u64)] });
            if rng.chance(25) {
                bg["overlay"] = json!(true);
            }
            bgs.push(bg);
        }
        model["backgrounds"] = Value::Array(bgs);
    }
    if rng.chance(50) {
        let mut pls = Vec::new();
        for p in 0..1 + rng.below(3) {
            let last = bars
                .last()
                .map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0));
            let price = if cfg.around_price {
                last * (0.97 + 0.06 * rng.unit())
            } else {
                rng.range(-20.0, 90.0)
            };
            let mut pl = json!({ "id": format!("{id}-p{p}"), "price": r4(price) });
            if rng.chance(60) {
                pl["color"] = json!(THEME_COLORS[rng.below(THEME_COLORS.len() as u64)]);
            }
            if rng.chance(60) {
                pl["lineStyle"] = json!(STYLES[rng.below(3)]);
            }
            if rng.chance(50) {
                pl["width"] = json!(rng.pick(&[0.5, 1.0, 2.0, 3.0]));
            }
            pls.push(pl);
        }
        model["priceLines"] = Value::Array(pls);
    }
    if cfg.around_price && n > 10 && rng.chance(35) {
        let bc: Vec<Value> = (0..1 + rng.below(40))
            .map(|_| json!({ "time": bar_time_at(bars, rng.below(n as u64) as i64, interval), "color": THEME_COLORS[rng.below(THEME_COLORS.len() as u64)] }))
            .collect();
        model["barColors"] = Value::Array(bc);
    }
    model
}

fn gen_trades(rng: &mut Rng, bars: &[Value]) -> Vec<Value> {
    let n = bars.len();
    if n < 10 {
        return vec![];
    }
    let mut out = Vec::new();
    for t in 0..2 + rng.below(8) {
        let i = rng.below(n as u64);
        let b = &bars[i];
        let (lo, hi) = (b[3].as_f64().unwrap_or(1.0), b[2].as_f64().unwrap_or(2.0));
        let mut e = json!({
            "time": b[0], "price": r4(lo + (hi - lo) * rng.unit()),
            "side": if rng.chance(50) { "buy" } else { "sell" },
            "kind": if rng.chance(60) { "entry" } else { "exit" },
        });
        if rng.chance(70) {
            e["label"] = json!(["Long", "Short", "Exit #4", "TP", ""][rng.below(5)]);
        }
        if rng.chance(70) {
            e["qty"] = json!(rng.pick(&[1.0, 0.5, 12.0, 100.0, 0.0001]));
        }
        e["tradeId"] = json!(format!("t{t}"));
        out.push(e.clone());
        // A second fill on the same bar now and then (they stack outward).
        if rng.chance(25) {
            let mut e2 = e.clone();
            e2["kind"] = json!("exit");
            out.push(e2);
        }
    }
    out
}

fn gen_marks(rng: &mut Rng, bars: &[Value], interval: i64) -> (Vec<Value>, Vec<Value>) {
    let n = bars.len();
    if n < 4 {
        return (vec![], vec![]);
    }
    let groups = vec![
        json!({ "id": "dividends", "label": "Dividends" }),
        json!({ "id": "splits", "label": "Splits", "visible": false }),
        json!({ "id": "news", "label": "News" }),
        json!({ "id": "latest", "label": "Latest", "parent": "news" }),
    ];
    let letters = [
        "D",
        "E",
        "!",
        "MM",
        "CH",
        "e{0301}",
        "{1F44D}",
        "AB",
        "S",
        "i",
        "",
        "ABC",
        "{1F600}{1F600}{1F600}",
        "e{0301}{0301}x",
        "WW",
        "{1F468}{200D}{1F469}x",
    ];
    let shapes = ["circle", "square", "diamond", "pin"];
    let colors = [
        "#2962ff",
        "#ff9800",
        "#e91e63",
        "#26a69a",
        "#ffffff",
        "#000000",
        "rgba(120,120,120,0.9)",
    ];
    let mut marks = Vec::new();
    let many = if rng.chance(20) { 70 } else { 30 };
    for m in 0..3 + rng.below(many) {
        let slot = if rng.chance(8) {
            n as i64 + rng.below(6) as i64
        } else {
            rng.below(n as u64) as i64
        };
        let mut time = bar_time_at(bars, slot, interval)
            + if rng.chance(30) {
                rng.below(interval as u64) as i64
            } else {
                0
            };
        if rng.chance(5) {
            time = bars[0][0].as_i64().unwrap() - interval * 3;
        }
        let mut glyph = json!({ "color": colors[rng.below(colors.len() as u64)] });
        if rng.chance(75) {
            glyph["shape"] = json!(shapes[rng.below(4)]);
        }
        match rng.below(5) {
            0 => glyph["icon"] = json!(if rng.chance(15) { "" } else { "dividend" }),
            1 => {}
            _ => glyph["letter"] = json!(letters[rng.below(letters.len() as u64)]),
        }
        let mut mark = json!({ "id": format!("m{m}"), "time": time, "glyph": glyph });
        if rng.chance(75) {
            mark["group"] =
                json!(["dividends", "splits", "news", "latest", "unknown"][rng.below(5)]);
        }
        if rng.chance(30) {
            mark["tooltip"] = json!("tip");
        }
        marks.push(mark);
    }
    (marks, groups)
}

/// Pixel layout of the panes inside `height`: price weight 3, studies 1, a collapsed one a fixed strip.
fn layout_panes(rng: &mut Rng, count: usize, height: f64, collapse: bool) -> Vec<Value> {
    let mut specs: Vec<(String, &str, f64, bool)> =
        vec![("price".to_string(), "price", 3.0, false)];
    for k in 1..count {
        specs.push((
            format!("study{k}"),
            "study",
            rng.pick(&[1.0, 1.0, 2.0]),
            collapse && k == count - 1,
        ));
    }
    let collapsed_px = 22.0;
    let flexible: f64 = specs.iter().filter(|s| !s.3).map(|s| s.2).sum();
    let used: f64 = specs.iter().filter(|s| s.3).count() as f64 * collapsed_px;
    let mut top = 0.0;
    specs
        .iter()
        .enumerate()
        .map(|(order, (id, kind, weight, collapsed))| {
            let h = if *collapsed { collapsed_px } else { ((height - used) * weight / flexible * 2.0).round() / 2.0 };
            let pane = json!({ "id": id, "kind": kind, "order": order, "heightWeight": weight, "top": top, "height": h, "collapsed": collapsed });
            top += h;
            pane
        })
        .collect()
}

fn gen_frame(
    rng: &mut Rng,
    datasets: &[Value],
    focus: Focus,
    dataset: Option<usize>,
    n_frame: usize,
) -> Value {
    let ds = dataset.unwrap_or_else(|| match rng.below(15) {
        0 => DS_SESSIONS,
        1 => DS_DAILY,
        2 => DS_TINY,
        3 => DS_BIG,
        4 => DS_QUIRKS,
        5 => DS_SECONDS,
        6 => DS_WEEKLY,
        7 => DS_UNEVEN,
        8 => DS_SESSIONS,
        9 => DS_NAN,
        10 => DS_SIGNED,
        11 => DS_SIGNED,
        _ => DS_MIN1,
    });
    let bars = datasets[ds]["bars"].as_array().unwrap();
    let interval = datasets[ds]["interval"].as_i64().unwrap();
    let n = bars.len();
    let dpr = rng.pick(&[1.0, 1.0, 2.0, 1.5, 2.0, 1.25, 3.0, 0.75]);
    let width = rng.pick(&[480.0, 640.0, 900.0, 1100.0, 1400.0]);
    let height = rng.pick(&[260.0, 400.0, 520.0, 640.0]);
    let spacing = if ds == DS_DENSE {
        rng.pick(&[0.15, 0.2, 0.3, 0.5, 0.9, 1.0, 2.5, 3.0])
    } else {
        rng.pick(&[
            0.4, 0.8, 1.0, 1.5, 2.0, 2.9, 3.0, 4.0, 6.0, 8.0, 8.0, 12.0, 20.0, 40.0, 80.0,
        ])
    };
    let right_offset = rng.pick(&[0.0, 3.0, 6.0, 6.0, 10.0, 25.0, -2.0, 40.0]);
    let pane_count = match focus {
        Focus::Chrome | Focus::Backend => 1 + rng.below(3),
        _ => 1 + rng.below(2),
    };
    let collapse = rng.chance(12) && pane_count > 1;
    let mut panes = layout_panes(rng, pane_count, height, collapse);
    let (from, to) = visible_window(n, width, spacing * rng.pick(&[1.0, 1.0, 1.0]), right_offset);
    let (lo, hi) = price_range(bars, from, to);
    let pad = (hi - lo) * 0.1;
    // Scale modes: percent/indexed baselines come from the first visible close.
    let i0 = from.floor().max(0.0) as usize;
    let first_close = bars
        .get(i0.min(n.saturating_sub(1)))
        .map_or(100.0, |b| b[4].as_f64().unwrap_or(100.0));
    let log_scale = focus == Focus::Chrome && rng.chance(25) && lo > 0.0;
    let invert = rng.chance(8);
    let scale_mode = if focus == Focus::Chrome || rng.chance(10) {
        rng.pick(&["price", "price", "percent", "indexed"])
    } else {
        "price"
    };
    for (k, p) in panes.iter_mut().enumerate() {
        if k == 0 {
            let (smin, smax) = if log_scale {
                (lo * 0.97, hi * 1.03)
            } else {
                (lo - pad, hi + pad)
            };
            let mut sc = json!({ "min": smin, "max": smax });
            if log_scale {
                sc["log"] = json!(true);
            }
            if invert {
                sc["invert"] = json!(true);
            }
            p["scale"] = sc;
            p["percentBaseline"] = json!(first_close);
        } else {
            let lo2 = rng.pick(&[-60.0, 0.0, -5.0, 0.0, 0.0, -1e6, 1e-6]);
            let hi2: f64 = lo2
                + rng.pick(&[
                    100.0, 100.0, 40.0, 1000.0, 3.0, 5e6, 2.5e9, 12_345.0, 0.001, 8e12, 999.5,
                    1500.0,
                ]);
            let mut sc = json!({ "min": lo2, "max": hi2 });
            let study_log = focus == Focus::Chrome && rng.chance(10);
            if study_log {
                sc = json!({ "min": 1.0, "max": hi2.max(2.0), "log": true });
                p["logScale"] = json!(true);
            }
            if rng.chance(10) {
                p["invert"] = json!(true);
                sc["invert"] = json!(true);
            }
            p["scale"] = sc;
            if focus == Focus::Chrome && rng.chance(25) {
                p["scaleMode"] = json!(rng.pick(&["percent", "indexed", "price"]));
                p["percentBaseline"] = json!(rng.pick(&[50.0, 0.0, 12.5]));
            }
            match rng.below(10) {
                0 => p["axisFormat"] = json!("volume"),
                1 => p["axisFormat"] = json!("none"),
                2 => {
                    p["axisBands"] = json!([{ "frac": 0.2, "label": "Low" }, { "frac": 0.5, "label": "Mid" }, { "frac": 0.8, "label": "High" }, { "frac": 0.99, "label": "Edge" }]);
                    p["axisFormat"] = json!("none");
                }
                _ => {}
            }
        }
    }

    // Scene flags and style.
    let mut scene = Map::new();
    let price_styles = [
        "candles",
        "candles",
        "candles",
        "bars",
        "line",
        "linemarkers",
        "stepline",
        "area",
        "baseline",
        "columns",
        "heikinashi",
    ];
    let styles_for_backend = focus == Focus::Backend || rng.chance(30);
    let price_style = if styles_for_backend {
        price_styles[rng.below(price_styles.len() as u64)]
    } else {
        "candles"
    };
    scene.insert("priceStyle".into(), json!(price_style));
    if rng.chance(4) {
        scene.insert("basePainting".into(), json!("none"));
    }
    if price_style == "baseline" && rng.chance(50) {
        scene.insert(
            "baselineValue".into(),
            json!(r4((lo + hi) / 2.0 + (hi - lo) * rng.range(-0.6, 0.6))),
        );
    }
    scene.insert("showPriceLine".into(), json!(!rng.chance(15)));
    scene.insert("showPriceLabel".into(), json!(!rng.chance(15)));
    scene.insert("showCountdown".into(), json!(!rng.chance(35)));
    scene.insert("logScale".into(), json!(log_scale));
    scene.insert("invertScale".into(), json!(invert));
    scene.insert("scaleMode".into(), json!(scale_mode));
    scene.insert("percentBaseline".into(), json!(first_close));
    if rng.chance(35) {
        scene.insert(
            "priceMintick".into(),
            json!(rng.pick(&[0.01, 0.5, 0.0001, 1.0, 0.0000001, 5.0])),
        );
    }
    if focus == Focus::Chrome || focus == Focus::Backdrop {
        scene.insert(
            "timezone".into(),
            json!(rng.pick(&["UTC", "UTC", "+05:30", "-03:00", "Etc/GMT+5", "+09:00"])),
        );
    }
    if focus != Focus::Backend && rng.chance(25) {
        scene.insert(
            "format".into(),
            json!({ "locale": rng.pick(&["en", "en-US"]), "numberingSystem": "latn", "hourCycle": rng.pick(&["h23", "h12", "locale"]), "english": true }),
        );
    }
    scene.insert(
        "showGrid".into(),
        json!(!rng.chance(if focus == Focus::Backdrop { 25 } else { 8 })),
    );
    scene.insert("showAxisLabels".into(), json!(!rng.chance(8)));
    if rng.chance(8) {
        scene.insert("candleZ".into(), json!(rng.pick(&[-1.0, -2.0, 1.0, -3.0])));
    }
    if rng.chance(6) {
        scene.insert("candlesHidden".into(), json!(true));
    }
    // Style patch.
    let mut style = Map::new();
    if rng.chance(30) {
        style.insert(
            "fontSize".into(),
            json!(rng.pick(&[9.0, 10.0, 12.0, 13.0, 14.0])),
        );
    }
    if rng.chance(25) {
        style.insert("gridVert".into(), json!({ "visible": !rng.chance(30), "color": if rng.chance(50) { json!("#ff000055") } else { Value::Null } }));
    }
    if rng.chance(25) {
        style.insert("gridHorz".into(), json!({ "visible": !rng.chance(30), "color": if rng.chance(50) { json!("rgba(0,255,0,0.2)") } else { Value::Null } }));
    }
    if rng.chance(25) {
        style.insert("borderColor".into(), json!("#ff00ff"));
    }
    if rng.chance(25) {
        style.insert("separatorColor".into(), json!("#00ffff"));
    }
    if rng.chance(40) {
        let mut c = Map::new();
        if rng.chance(50) {
            c.insert(
                "color".into(),
                if rng.chance(70) {
                    json!("#ffff00")
                } else {
                    Value::Null
                },
            );
        }
        if rng.chance(50) {
            c.insert("width".into(), json!(rng.pick(&[1.0, 2.0, 3.0, 4.0, 1.5])));
        }
        if rng.chance(50) {
            c.insert("style".into(), json!(STYLES[rng.below(3)]));
        }
        if rng.chance(50) {
            c.insert("opacity".into(), json!(rng.pick(&[0.2, 0.4, 1.0, 0.75])));
        }
        if rng.chance(50) {
            c.insert(
                "labelBackground".into(),
                if rng.chance(60) {
                    json!("#ffffff")
                } else {
                    Value::Null
                },
            );
        }
        style.insert("crosshair".into(), Value::Object(c));
    }
    if styles_for_backend || rng.chance(20) {
        if rng.chance(50) {
            style.insert(
                "candle".into(),
                json!({
                    "bodyVisible": !rng.chance(20), "borderVisible": rng.chance(50),
                    "borderUpColor": if rng.chance(50) { json!("#00ff00") } else { Value::Null },
                    "borderDownColor": if rng.chance(50) { json!("#ff0000") } else { Value::Null },
                    "wickVisible": !rng.chance(15),
                    "wickUpColor": if rng.chance(40) { json!("#0f0") } else { Value::Null },
                    "wickDownColor": if rng.chance(40) { json!("#f00") } else { Value::Null },
                }),
            );
        }
        if rng.chance(30) {
            style.insert("bars".into(), json!({ "upColor": if rng.chance(60) { json!("#00e676") } else { Value::Null }, "downColor": if rng.chance(60) { json!("#ff1744") } else { Value::Null } }));
        }
        if rng.chance(40) {
            style.insert("line".into(), json!({ "color": if rng.chance(70) { json!("#ffa726") } else { Value::Null }, "width": rng.pick(&[0.5, 1.0, 2.0, 4.0]) }));
        }
        if rng.chance(40) {
            style.insert("area".into(), json!({
                "lineColor": if rng.chance(70) { json!("#42a5f5") } else { Value::Null }, "width": rng.pick(&[1.0, 2.0, 3.0]),
                "topColor": if rng.chance(60) { json!("rgba(66,165,245,0.4)") } else { Value::Null },
                "bottomColor": if rng.chance(60) { json!("rgba(66,165,245,0)") } else { Value::Null },
            }));
        }
        if rng.chance(50) {
            style.insert("baseline".into(), json!({
                "topLineColor": if rng.chance(50) { json!("#00c853") } else { Value::Null },
                "bottomLineColor": if rng.chance(50) { json!("#d50000") } else { Value::Null },
                "topFillColor": if rng.chance(40) { json!("rgba(0,200,83,0.3)") } else { Value::Null },
                "topFillColor2": if rng.chance(40) { json!("rgba(0,200,83,0.02)") } else { Value::Null },
                "bottomFillColor": if rng.chance(40) { json!("rgba(213,0,0,0.3)") } else { Value::Null },
                "bottomFillColor2": if rng.chance(40) { json!("rgba(213,0,0,0.02)") } else { Value::Null },
                "width": rng.pick(&[1.0, 2.0, 3.0]), "baselineLevel": rng.pick(&[50.0, 0.0, 100.0, 25.0, 80.0, 120.0, -10.0]),
            }));
        }
        if rng.chance(40) {
            style.insert("columns".into(), json!({ "upColor": if rng.chance(60) { json!("#26a69a") } else { Value::Null }, "downColor": if rng.chance(60) { json!("rgba(239,83,80,0.7)") } else { Value::Null } }));
        }
    }
    if rng.chance(20) {
        style.insert("sessions".into(), json!({ "premarketColor": "rgba(255,152,0,0.12)", "postmarketColor": "#2962ff22", "extendedColor": "rgba(171,71,188,0.15)" }));
    }
    scene.insert("style".into(), Value::Object(style));
    if (price_style == "candles" || price_style == "heikinashi") && rng.chance(15) {
        scene.insert("candleOverride".into(), json!({
            "upColor": if rng.chance(50) { json!("#00bfa5") } else { Value::Null }, "downColor": if rng.chance(50) { json!("#dd2c00") } else { Value::Null },
            "bodyVisible": if rng.chance(50) { json!(rng.chance(50)) } else { Value::Null },
            "borderVisible": if rng.chance(50) { json!(rng.chance(50)) } else { Value::Null },
            "borderUpColor": if rng.chance(30) { json!("#1b5e20") } else { Value::Null },
            "borderDownColor": if rng.chance(30) { json!("#b71c1c") } else { Value::Null },
            "wickVisible": if rng.chance(30) { json!(rng.chance(60)) } else { Value::Null },
            "wickUpColor": if rng.chance(30) { json!("#a5d6a7") } else { Value::Null },
            "wickDownColor": if rng.chance(30) { json!("#ef9a9a") } else { Value::Null },
        }));
    }

    // Highlights and session zones.
    if (focus == Focus::Backdrop || rng.chance(12)) && n > 10 {
        let mut hs = Vec::new();
        for _ in 0..1 + rng.below(4) {
            let a = rng.below(n as u64) as i64;
            let len = 1 + rng.below(60) as i64;
            let sub = if rng.chance(40) { interval / 3 } else { 0 };
            hs.push(json!({ "from": bar_time_at(bars, a, interval) + sub, "to": bar_time_at(bars, a + len, interval) - sub, "color": FILL_COLORS[rng.below(FILL_COLORS.len() as u64)] }));
        }
        if rng.chance(15) {
            hs.push(json!({ "from": bar_time_at(bars, 5, interval), "to": bar_time_at(bars, 5, interval), "color": "#ff0000" }));
        }
        scene.insert("highlights".into(), Value::Array(hs));
    }
    if (focus == Focus::Backdrop || rng.chance(10)) && n > 10 {
        let mut zones = json!({ "pre": [], "post": [], "extended": [] });
        let mut at = -3i64;
        for _ in 0..2 + rng.below(6) {
            at += rng.below((n as u64 / 4).max(3)) as i64;
            let len = 1 + rng.below(12) as i64;
            let from =
                bar_time_at(bars, at, interval) - if rng.chance(30) { interval / 2 } else { 0 };
            let to = bar_time_at(bars, at + len, interval)
                + if rng.chance(30) { interval / 4 } else { 0 };
            let key = rng.pick(&["pre", "post", "extended"]);
            zones[key].as_array_mut().unwrap().push(json!([from, to]));
        }
        scene.insert("sessionZones".into(), zones);
    }

    // Indicators.
    let mut indicators = Vec::new();
    if matches!(focus, Focus::Backend) || rng.chance(30) {
        let count = if focus == Focus::Backend {
            rng.below(4) + (n_frame % 2)
        } else {
            1
        };
        for k in 0..count {
            let pane_ix = if pane_count > 1 && rng.chance(55) {
                1 + rng.below(pane_count as u64 - 1)
            } else {
                0
            };
            let pane_id = panes[pane_ix]["id"].as_str().unwrap().to_string();
            let mut m = gen_model(
                rng,
                format!("ind{k}"),
                &ModelCfg {
                    pane: pane_id,
                    around_price: pane_ix == 0,
                },
                bars,
                interval,
            );
            if rng.chance(20) {
                m["z"] = json!(rng.pick(&[-5.0, -1.0, 0.0, 1.0, 3.0]));
            }
            if pane_ix == 0 && rng.chance(12) {
                m["ownScale"] = json!(true);
                m["ownScaleWindow"] = json!({ "min": 0.0, "max": rng.pick(&[100.0, 50.0]) });
            }
            indicators.push(m);
        }
    }
    if focus == Focus::Chrome && rng.chance(25) {
        // Merged (own-scale) columns on the axis, on any pane.
        let pane_ix = rng.below(pane_count as u64);
        let pane_id = panes[pane_ix]["id"].as_str().unwrap().to_string();
        let mut m = gen_model(
            rng,
            format!("merged{}", indicators.len()),
            &ModelCfg {
                pane: pane_id,
                around_price: false,
            },
            bars,
            interval,
        );
        m["ownScale"] = json!(true);
        m["ownScaleWindow"] =
            json!({ "min": rng.pick(&[0.0, -10.0]), "max": rng.pick(&[100.0, 7.5, 12345.0]) });
        indicators.push(m);
    }
    if (focus == Focus::Chrome && rng.chance(40)) || rng.chance(8) {
        let trades = gen_trades(rng, bars);
        if indicators.is_empty() {
            let mut m = json!({ "id": "strategy", "paneId": "price", "series": [], "fills": [] });
            m["trades"] = Value::Array(trades);
            indicators.push(m);
        } else {
            indicators[0]["trades"] = Value::Array(trades);
        }
        if rng.chance(30) {
            scene.insert("tradeMarkers".into(), json!({ "visible": !rng.chance(30), "labels": !rng.chance(30), "qty": !rng.chance(30) }));
        }
    }
    // Timeline marks.
    if (focus == Focus::Chrome && rng.chance(40)) || rng.chance(5) {
        let (marks, groups) = gen_marks(rng, bars, interval);
        scene.insert("timelineMarks".into(), Value::Array(marks.clone()));
        scene.insert("markGroups".into(), Value::Array(groups));
        let mut st = json!({ "visible": !rng.chance(10), "groups": {} });
        if rng.chance(40) {
            st["groups"] = json!({ "dividends": rng.chance(70), "news": rng.chance(60) });
        }
        scene.insert("marks".into(), st);
        if let Some(first) = marks.first() {
            let t = first["time"].as_i64().unwrap();
            let times: Vec<i64> = bars.iter().map(|b| b[0].as_i64().unwrap()).collect();
            if let Some(bar) = snap_mark_bar(t, &times, interval as f64) {
                let key = format!("{bar}|{}", first["group"].as_str().unwrap_or(""));
                if rng.chance(40) {
                    scene.insert("marksHoverKey".into(), json!(key));
                    scene.insert(
                        "marksHoverSince".into(),
                        json!(rng.pick(&[1000.0, 900.0, 1.0, 600.0])),
                    );
                }
                if rng.chance(25) {
                    scene.insert("marksActiveKey".into(), json!(key));
                }
                if rng.chance(25) {
                    scene.insert(
                        "marksFlash".into(),
                        json!({ "key": key, "until": rng.pick(&[1100.0, 900.0, 1000.0]) }),
                    );
                }
                if rng.chance(25) {
                    scene.insert("marksExpandedStack".into(), json!(bar));
                }
            }
        }
    }

    // Crosshair.
    let data_w = width;
    let data_h = height;
    let ch_roll = rng.below(10);
    if focus == Focus::Crosshair || ch_roll < 4 {
        let pane = &panes[rng.below(panes.len() as u64)];
        let (top, h) = (
            pane["top"].as_f64().unwrap(),
            pane["height"].as_f64().unwrap(),
        );
        let x = match rng.below(8) {
            0 => 0.0,
            1 => data_w,
            2 => data_w + 0.5,
            3 => -1.0,
            _ => (rng.unit() * data_w * 4.0).round() / 4.0,
        };
        let y = match rng.below(8) {
            0 => top,
            1 => top + h,
            2 => 0.0,
            3 => data_h,
            4 => data_h + 3.0,
            _ => top + (rng.unit() * h * 4.0).round() / 4.0,
        };
        scene.insert("crosshair".into(), json!({ "x": x, "y": y }));
    }
    if focus == Focus::Crosshair && rng.chance(45) {
        let mut ov = Map::new();
        if rng.chance(40) {
            ov.insert("vertical".into(), json!(rng.chance(50)));
        }
        if rng.chance(40) {
            ov.insert("horizontal".into(), json!(rng.chance(50)));
        }
        if rng.chance(40) {
            ov.insert("color".into(), json!("#ff4081"));
        }
        if rng.chance(40) {
            ov.insert("width".into(), json!(rng.pick(&[0.5, 2.0, 3.0, 8.0])));
        }
        if rng.chance(40) {
            ov.insert("style".into(), json!(STYLES[rng.below(3)]));
        }
        if rng.chance(40) {
            ov.insert("opacity".into(), json!(rng.pick(&[0.1, 0.9, 1.0])));
        }
        if rng.chance(50) {
            ov.insert(
                "shadeRight".into(),
                if rng.chance(50) {
                    json!({ "color": "rgba(0,0,0,0.5)", "opacity": 0.6 })
                } else {
                    json!({ "color": "#00000066" })
                },
            );
        }
        scene.insert("crosshairOverride".into(), Value::Object(ov));
    }
    let mut external = Value::Null;
    if focus == Focus::Crosshair && rng.chance(55) && n > 0 {
        let bar = rng.below(n as u64) as i64;
        let x = (rng.unit() * data_w * 4.0).round() / 4.0;
        let y_pick = rng.below(4);
        let y = if y_pick == 0 {
            Value::Null
        } else {
            json!((rng.unit() * data_h * 2.0).round() / 2.0)
        };
        let mut e = json!({ "x": x, "y": y, "time": bar_time_at(bars, bar, interval) });
        if rng.chance(70) {
            e["price"] = json!(r4(lo + (hi - lo) * rng.unit()));
        }
        if rng.chance(25) {
            e["line"] = json!(false);
        }
        if rng.chance(12) {
            e["x"] = json!(rng.pick(&[-30.0, data_w + 40.0]));
        }
        external = e;
    }
    let separator_hover = if focus == Focus::Crosshair && rng.chance(35) && panes.len() > 1 {
        json!(panes[1]["top"])
    } else {
        Value::Null
    };

    // Clock: the countdown reads the wall clock against the last bar's span.
    let last_time = bars.last().map_or(0, |b| b[0].as_i64().unwrap());
    let wall = last_time
        + (interval as f64 * rng.pick(&[0.0, 0.1, 0.5, 0.9, 0.999, 1.0, 1.5, 7.0])) as i64;
    let surface = if rng.chance(55) {
        json!({ "background": rng.pick(&["#101010", "#ffffff", "#0b0e11"]), "textColor": rng.pick(&["#eaeaea", "#222", "#b2b5be"]) })
    } else {
        Value::Null
    };
    let theme_ix = rng.below(4);
    let slices = if rng.chance(10) && !panes.is_empty() {
        json!({ "price": [rng.pick(&[-2.0, 0.0, 1.0]), rng.pick(&[0.0, 2.0])], panes[panes.len() - 1]["id"].as_str().unwrap(): [Value::Null] })
    } else {
        json!({})
    };
    let gutter = 64.0
        + 56.0
            * indicators
                .iter()
                .filter(|m| m["ownScale"] == json!(true))
                .count() as f64;
    let mut spec = json!({
        "dataset": ds, "theme": theme_ix, "dpr": dpr, "width": width, "height": height,
        "fullWidth": width + gutter, "fullHeight": height + 28.0,
        "viewport": { "barSpacing": spacing, "rightOffset": right_offset },
        "panes": panes, "scene": Value::Object(scene), "indicators": indicators, "slices": slices,
        "clock": { "nowMs": rng.pick(&[1000.0, 1200.0, 1100.0, 5000.0]), "wallMs": wall },
        "surface": surface, "external": external, "separatorHoverY": separator_hover,
        "gridAlpha": rng.pick(&[1.0, 1.0, 0.5, 0.0, 0.3]),
        "alphas": { "model": rng.pick(&[1.0, 1.0, 0.6, 0.0]), "body": rng.pick(&[1.0, 1.0, 0.4, 0.0]), "structure": rng.pick(&[1.0, 1.0, 0.7]), "scale": rng.pick(&[1.0, 1.0, 0.8, 0.5]) },
    });
    if rng.chance(6) {
        spec["pitchScale"] = json!(rng.pick(&[0.5, 1.5, 2.0, 0.8]));
    }
    spec
}

/// A backend frame pinned to one price style at one zoom, so every level-of-detail tier is met by
/// every candle-like style (the random frames only meet them by chance).
fn lod_frame(rng: &mut Rng, datasets: &[Value], k: usize) -> Value {
    let styles = ["candles", "bars", "heikinashi", "columns", "line", "area"];
    let spacings = [
        0.15, 0.4, 0.8, 0.99, 1.0, 1.5, 2.9, 3.0, 5.0, 8.0, 20.0, 60.0,
    ];
    let ds = rng.pick(&[
        DS_DENSE,
        DS_DENSE,
        DS_MIN1,
        DS_QUIRKS,
        DS_SESSIONS,
        DS_DAILY,
        DS_NAN,
        DS_UNEVEN,
    ]);
    let mut spec = gen_frame(rng, datasets, Focus::Backend, Some(ds), k);
    spec["scene"]["priceStyle"] = json!(styles[k % styles.len()]);
    spec["viewport"]["barSpacing"] = json!(spacings[(k / styles.len()) % spacings.len()]);
    if let Some(sc) = spec["scene"].as_object_mut() {
        sc.remove("candleOverride");
        sc.insert("candlesHidden".into(), json!(false));
        sc.insert("basePainting".into(), json!("candles"));
    }
    if k.is_multiple_of(3) {
        let bars = datasets[ds]["bars"].as_array().unwrap();
        let mut colors: Vec<Value> = Vec::new();
        for b in bars {
            if rng.chance(12) {
                colors.push(json!({ "time": b[0], "color": THEME_COLORS[rng.below(THEME_COLORS.len() as u64)] }));
            }
        }
        spec["indicators"].as_array_mut().unwrap().push(json!({ "id": "recolor", "paneId": "price", "series": [], "fills": [], "barColors": colors }));
    }
    spec
}

/// Frames that reach the corners the random ones only reach by luck: each pins the state that
/// exposes one known behavior of the two painters (see the parity script's notes).
fn edge_frames(rng: &mut Rng, datasets: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    // A zoom-out so far that the year ticks run through the years 0 to 99 (JavaScript's Date.UTC
    // reads those as 1900 to 1999) and past them.
    for (ds, spacings) in [
        (DS_DAILY, vec![0.001, 1e-5]),
        (DS_WEEKLY, vec![0.001, 1e-5]),
        (DS_MIN1, vec![0.001]),
    ] {
        for spacing in spacings {
            let mut spec = gen_frame(rng, datasets, Focus::Chrome, Some(ds), 0);
            spec["viewport"] = json!({ "barSpacing": spacing, "rightOffset": 40.0 });
            spec["edge"] = json!("year-ticks-through-year-0");
            out.push(spec);
        }
    }
    // A viewport that makes the visible range NaN (zero spacing): with and without drawing slices.
    for with_slices in [false, true] {
        let mut spec = gen_frame(rng, datasets, Focus::Backend, Some(DS_MIN1), 0);
        spec["viewport"]["barSpacing"] = json!(0.0);
        spec["slices"] = if with_slices {
            json!({ "price": [0.0, 2.0] })
        } else {
            json!({})
        };
        spec["edge"] = json!("nan-visible-range");
        out.push(spec);
    }
    // A crosshair whose snapped bar has no valid time, under each hour-cycle setting.
    for cycle in ["h23", "h12", "locale"] {
        for with_format in [true, false] {
            let mut spec = gen_frame(rng, datasets, Focus::Crosshair, Some(DS_MIN1), 0);
            spec["viewport"]["barSpacing"] = json!(0.0);
            spec["scene"]["crosshair"] = json!({ "x": 400.5, "y": 120.25 });
            if with_format {
                spec["scene"]["format"] = json!({ "locale": "en", "numberingSystem": "latn", "hourCycle": cycle, "english": true });
            }
            spec["edge"] = json!("crosshair-on-an-invalid-time");
            out.push(spec);
        }
    }
    out
}

// ───────────────────────────── painters ─────────────────────────────

fn surface_of(spec: &Value) -> Option<AxisSurface> {
    spec.get("surface")
        .filter(|s| !s.is_null())
        .map(|s| AxisSurface {
            background: string(s, "background"),
            text_color: string(s, "textColor"),
        })
}

fn run_frame(spec: &Value, datasets: &[Vec<Ohlcv>], themes: &[Value]) -> Value {
    let f = build_frame(spec, datasets, themes);
    let backing = (f.full_w * f.dpr, f.full_h * f.dpr);
    let mut ops = Map::new();

    // Geometry backend.
    let canvas = recording();
    let mut backend = Canvas2dBackend::new();
    backend.mount(canvas.clone(), backing.0, backing.1);
    let a = &spec["alphas"];
    backend.set_model_alpha(num(a, "model"));
    backend.set_candle_body_alpha(num(a, "body"));
    backend.set_candle_structure_alpha(num(a, "structure"));
    backend.set_candle_body_scale(num(a, "scale"));
    backend.render(&f.scene, &f.coords, &f.theme);
    ops.insert("backend".into(), canvas.to_json());

    // Backdrop and chrome share one tick source, as in the app.
    let ticks = Rc::new(PriceAxisTickSource::new());
    let canvas = recording();
    let mut backdrop = BackdropRenderer::new(ticks.clone());
    backdrop.mount(canvas.clone(), backing.0, backing.1);
    backdrop.render(&f.scene, &f.coords, &f.theme, num(spec, "gridAlpha"));
    ops.insert("backdrop".into(), canvas.to_json());

    let canvas = recording();
    let mut chrome = ChromeRenderer::new(ticks);
    chrome.mount(canvas.clone(), backing.0, backing.1);
    let clock = FrameClock {
        now_ms: num(&spec["clock"], "nowMs"),
        wall_ms: num(&spec["clock"], "wallMs"),
    };
    chrome.render(
        &f.scene,
        &f.coords,
        &f.theme,
        surface_of(spec).as_ref(),
        &clock,
        &NoMarkIcons,
    );
    ops.insert("chrome".into(), canvas.to_json());

    let canvas = recording();
    let mut cross = CrosshairRenderer::new();
    cross.mount(canvas.clone(), backing.0, backing.1);
    let external = spec
        .get("external")
        .filter(|e| !e.is_null())
        .map(|e| ExternalCrosshair {
            x: num(e, "x"),
            y: opt_num(e, "y"),
            time: num(e, "time"),
            price: opt_num(e, "price"),
            line: opt_flag(e, "line"),
        });
    cross.render(
        &f.scene,
        &f.coords,
        &f.theme,
        opt_num(spec, "separatorHoverY"),
        external.as_ref(),
    );
    ops.insert("crosshair".into(), canvas.to_json());
    let shade = cross
        .shade_right_area()
        .map(|s| json!({ "x": s.x, "width": s.width, "height": s.height }));
    json!({ "spec": spec, "ops": ops, "shade": shade })
}

// ───────────────────────────── marks (direct lane) ─────────────────────────────

struct IconLog {
    calls: std::cell::RefCell<Vec<Value>>,
    ready: bool,
}

impl MarkIcons for IconLog {
    fn get(&self, icon: &str, ink: &str, px: f64, dpr: f64) -> Option<CanvasImage> {
        self.calls.borrow_mut().push(json!([icon, ink, px, dpr]));
        self.ready.then(|| Rc::new(()) as CanvasImage)
    }
}

fn run_marks(spec: &Value, datasets: &[Vec<Ohlcv>]) -> Value {
    let bars = &datasets[spec["dataset"].as_u64().unwrap() as usize];
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    let marks: Vec<Rc<TimelineMark>> = items(spec, "marks")
        .iter()
        .map(|m| Rc::new(mark_from_json(m)))
        .collect();
    let groups: Vec<MarkGroup> = items(spec, "groups").iter().map(group_from_json).collect();
    let hidden_groups: Vec<String> = items(spec, "hiddenGroups")
        .iter()
        .map(|g| g.as_str().unwrap().to_string())
        .collect();
    let mut coords = CoordinateSystem::new();
    coords.set_size(num(spec, "width"), num(spec, "height"), num(spec, "dpr"));
    coords.set_bars(&times);
    coords.set_viewport(ViewportState {
        bar_spacing: num(&spec["viewport"], "barSpacing"),
        right_offset: num(&spec["viewport"], "rightOffset"),
    });
    let hidden = |g: &str| hidden_groups.iter().any(|h| h == g);
    let layout = layout_mark_lane(&MarkLaneInput {
        marks: &marks,
        groups: &groups,
        hidden: &hidden,
        bar_times: &times,
        interval_ms: coords.bar_interval(),
        x_of: &|bar| coords.logical_to_x(bar as f64),
        axis_y: num(spec, "height"),
        data_w: num(spec, "width"),
        expanded: spec
            .get("expanded")
            .and_then(Value::as_u64)
            .map(|e| e as usize),
    });
    let glyphs: Vec<Value> = layout.glyphs.iter().map(|g| json!({ "key": g.cluster.key, "x": g.x, "y": g.y, "size": g.size, "stack": g.stack, "depth": g.depth, "decked": g.decked, "marks": g.cluster.marks.len() })).collect();
    let canvas = recording();
    let icons = IconLog {
        calls: Default::default(),
        ready: flag(spec, "iconsReady", false),
    };
    let intl = vela_core::intl::default_intl();
    let theme = &spec["colors"];
    paint_mark_lane(
        &*canvas,
        &layout,
        &MarkPaintDeps {
            axis_y: num(spec, "height"),
            background: theme["background"].as_str().unwrap(),
            stem_color: theme["stem"].as_str().unwrap(),
            font_family: theme["fontFamily"].as_str().unwrap(),
            dpr: num(spec, "dpr"),
            icons: &icons,
            intl: &*intl,
            hover_key: spec.get("hoverKey").and_then(Value::as_str),
            hover_since: num_or(spec, "hoverSince", 0.0),
            active_key: spec.get("activeKey").and_then(Value::as_str),
            flash_key: spec.get("flashKey").and_then(Value::as_str),
            now_ms: num(spec, "nowMs"),
        },
    );
    json!({ "spec": spec, "ops": canvas.to_json(), "glyphs": glyphs, "iconCalls": icons.calls.into_inner() })
}

fn gen_marks_scene(rng: &mut Rng, datasets: &[Value]) -> Value {
    let ds = rng.pick(&[
        DS_MIN1,
        DS_SESSIONS,
        DS_DAILY,
        DS_UNEVEN,
        DS_WEEKLY,
        DS_SECONDS,
        DS_THREE,
    ]);
    let bars = datasets[ds]["bars"].as_array().unwrap();
    let interval = datasets[ds]["interval"].as_i64().unwrap();
    let (marks, groups) = gen_marks(rng, bars, interval);
    let key_of = |rng: &mut Rng| -> Value {
        if marks.is_empty() {
            return Value::Null;
        }
        let m = &marks[rng.below(marks.len() as u64)];
        let times: Vec<i64> = bars.iter().map(|b| b[0].as_i64().unwrap()).collect();
        match snap_mark_bar(m["time"].as_i64().unwrap(), &times, interval as f64) {
            Some(bar) => json!(format!("{bar}|{}", m["group"].as_str().unwrap_or(""))),
            None => Value::Null,
        }
    };
    let hover = key_of(rng);
    let active = if rng.chance(35) {
        key_of(rng)
    } else {
        Value::Null
    };
    let flash = if rng.chance(25) {
        key_of(rng)
    } else {
        Value::Null
    };
    let hidden: Vec<&str> = ["splits", "news", "latest"]
        .into_iter()
        .filter(|_| rng.chance(25))
        .collect();
    let now = rng.pick(&[1000.0, 1100.0, 1360.0, 1180.0, 999.0, 1500.0]);
    let expanded = if rng.chance(30) {
        json!(rng.below(bars.len() as u64))
    } else {
        Value::Null
    };
    json!({
        "dataset": ds, "dpr": rng.pick(&[1.0, 2.0, 1.5]), "width": rng.pick(&[640.0, 900.0, 1200.0]), "height": rng.pick(&[300.0, 500.0]),
        "viewport": { "barSpacing": rng.pick(&[0.5, 1.0, 2.0, 4.0, 8.0, 14.0, 25.0, 60.0, 0.2, 0.05, 0.0, 3.0]), "rightOffset": rng.pick(&[0.0, 6.0, 20.0, 1e6, -50.0]) },
        "marks": marks, "groups": groups, "hiddenGroups": hidden, "expanded": expanded,
        "colors": { "background": rng.pick(&["#131722", "#ffffff"]), "stem": rng.pick(&["#363a45", "#bbbbbb"]), "fontFamily": rng.pick(&["Inter, sans-serif", "monospace"]) },
        "hoverKey": hover, "hoverSince": rng.pick(&[1000.0, 900.0, 1100.0, 1000.0 - 180.0, 700.0]),
        "activeKey": active, "flashKey": flash, "nowMs": now, "iconsReady": rng.chance(50),
    })
}

// ───────────────────────────── volume / VPVR ─────────────────────────────

fn gen_inputs_volume(rng: &mut Rng) -> Value {
    let mut m = Map::new();
    if rng.chance(50) {
        m.insert(
            "upColor".into(),
            json!(rng.pick(&["#26a69a", "#00e676", ""])),
        );
    }
    if rng.chance(50) {
        m.insert(
            "downColor".into(),
            json!(rng.pick(&["#ef5350", "rgba(255,0,0,0.6)", ""])),
        );
    }
    if rng.chance(70) {
        m.insert(
            "heightPct".into(),
            json!(rng.pick(&[20.0, 5.0, 50.0, 80.0, 1.0, 33.0])),
        );
    }
    Value::Object(m)
}

fn gen_inputs_vpvr(rng: &mut Rng) -> Value {
    let mut m = Map::new();
    if rng.chance(70) {
        m.insert(
            "rows".into(),
            json!(rng.pick(&[24.0, 2.0, 5.0, 60.0, 400.0, 1.0, 999.0, 17.0, 12.4])),
        );
    }
    if rng.chance(70) {
        m.insert(
            "widthPct".into(),
            json!(rng.pick(&[30.0, 5.0, 90.0, 50.0, 2.0, 100.0])),
        );
    }
    if rng.chance(40) {
        m.insert("upColor".into(), json!(rng.pick(&["#2962ff", "#00e676"])));
    }
    if rng.chance(40) {
        m.insert(
            "downColor".into(),
            json!(rng.pick(&["#f23645", "rgba(255,0,0,0.6)"])),
        );
    }
    if rng.chance(70) {
        m.insert("showPoc".into(), json!(rng.chance(60)));
    }
    if rng.chance(70) {
        m.insert(
            "valueAreaPct".into(),
            json!(rng.pick(&[70.0, 0.0, 100.0, 50.0, 95.0, 150.0, 30.0])),
        );
    }
    Value::Object(m)
}

fn inputs_from_json(v: &Value) -> Inputs {
    let mut out = Inputs::new();
    for (k, x) in v.as_object().into_iter().flatten() {
        out.insert(
            k.clone(),
            match x {
                Value::Number(n) => InputValue::Number(n.as_f64().unwrap()),
                Value::String(s) => InputValue::Text(s.clone()),
                Value::Bool(b) => InputValue::Bool(*b),
                _ => continue,
            },
        );
    }
    out
}

fn layer_json_volume(d: &VolumeLayerData) -> Value {
    serde_json::to_value(d).unwrap()
}
fn layer_json_vpvr(d: &VpvrLayerData) -> Value {
    serde_json::to_value(d).unwrap()
}

/// A volume or VPVR scenario: one frame spec plus the layer's input record, rendered through
/// the layer renderer (a sequence of frames on one instance, so the VPVR memo is exercised).
fn run_layer(spec: &Value, datasets: &[Vec<Ohlcv>], themes: &[Value], vpvr: bool) -> Value {
    let mut f = build_frame(spec, datasets, themes);
    let backing = (f.full_w * f.dpr, f.full_h * f.dpr);
    let canvas = recording();
    let inputs = inputs_from_json(&spec["inputs"]);
    let volume_data = volume_layer_data(&inputs);
    let vpvr_data = vpvr_layer_data(&inputs);
    let mut volume = VolumeRenderer::new();
    let mut profile = VpvrRenderer::new();
    volume.mount(canvas.clone(), backing.0, backing.1);
    profile.mount(canvas.clone(), backing.0, backing.1);
    let pane = f.scene.panes.get("price").unwrap().clone();
    let mut frames = Vec::new();
    for fr in items(spec, "frames") {
        f.coords.set_viewport(ViewportState {
            bar_spacing: num(fr, "barSpacing"),
            right_offset: num(fr, "rightOffset"),
        });
        let mut bars = f.scene.bars.clone();
        if let Some(m) = fr.get("mutateLast")
            && let Some(last) = bars.last_mut()
        {
            last.high = num(m, "high");
            last.close = num(m, "close");
            last.volume = opt_num(m, "volume");
        }
        let start = canvas.ops().len();
        if vpvr {
            profile.render(&VpvrRenderArgs {
                bars: &bars,
                data: if flag(fr, "noData", false) {
                    None
                } else {
                    Some(&vpvr_data)
                },
                visible: flag(fr, "visible", true),
                coords: &f.coords,
                scale: pane.scale,
                bounds: PaneBounds {
                    top: num(fr, "top"),
                    height: num(fr, "boundsHeight"),
                },
                theme: &f.theme,
            });
        } else {
            volume.render(&VolumeRenderArgs {
                bars: &bars,
                data: if flag(fr, "noData", false) {
                    None
                } else {
                    Some(&volume_data)
                },
                visible: flag(fr, "visible", true),
                coords: &f.coords,
                bounds: PaneBounds {
                    top: num(fr, "top"),
                    height: num(fr, "boundsHeight"),
                },
                fill_pane: flag(fr, "fillPane", false),
            });
        }
        frames.push(canvas.to_json().as_array().unwrap()[start..].to_vec());
    }
    json!({
        "spec": spec, "frames": frames,
        "layer": if vpvr { layer_json_vpvr(&vpvr_data) } else { layer_json_volume(&volume_data) },
    })
}

fn gen_layer_spec(rng: &mut Rng, datasets: &[Value], vpvr: bool, n_frame: usize) -> Value {
    let dataset = if rng.chance(25) {
        Some(if rng.chance(50) { DS_QUIRKS } else { DS_UNEVEN })
    } else {
        None
    };
    let mut spec = gen_frame(rng, datasets, Focus::Chrome, dataset, n_frame);
    // Strip the chrome-only baggage so the spec is about the layer.
    let price_h = spec["panes"][0]["height"].as_f64().unwrap();
    let study = spec["panes"].as_array().unwrap().len() > 1;
    let mut frames = Vec::new();
    let base_spacing = spec["viewport"]["barSpacing"].as_f64().unwrap();
    let base_offset = spec["viewport"]["rightOffset"].as_f64().unwrap();
    let count = if vpvr {
        2 + rng.below(4)
    } else {
        1 + rng.below(2)
    };
    for k in 0..count {
        // The pane the layer lives in: the price pane, or (volume) a study pane filling it.
        let fill_pane = !vpvr && study && rng.chance(40);
        let (top, h) = if fill_pane {
            (
                spec["panes"][1]["top"].as_f64().unwrap(),
                spec["panes"][1]["height"].as_f64().unwrap(),
            )
        } else {
            (spec["panes"][0]["top"].as_f64().unwrap(), price_h)
        };
        let mut fr = json!({
            "barSpacing": if k == 0 { base_spacing } else { rng.pick(&[base_spacing, base_spacing, base_spacing * 1.5, 4.0, 8.0, 0.8]) },
            "rightOffset": if k == 0 { base_offset } else { rng.pick(&[base_offset, base_offset, 6.0, 0.0, 30.0]) },
            "top": top, "boundsHeight": h, "fillPane": fill_pane,
        });
        if k > 0 && rng.chance(25) {
            fr["mutateLast"] = json!({ "high": 9999.0, "close": 9000.0, "volume": rng.pick(&[1000.0, 0.0, 123456.0]) });
        }
        if rng.chance(8) {
            fr["visible"] = json!(false);
        }
        if rng.chance(6) {
            fr["noData"] = json!(true);
        }
        if rng.chance(6) {
            fr["boundsHeight"] = json!(0.0);
        }
        frames.push(fr);
    }
    spec["inputs"] = if vpvr {
        gen_inputs_vpvr(rng)
    } else {
        gen_inputs_volume(rng)
    };
    if rng.chance(25) {
        match rng.below(6) {
            0 => spec["panes"][0]["scale"] = json!({ "min": 100.0, "max": 100.0 }),
            1 => spec["panes"][0]["scale"] = json!({ "min": 150.0, "max": 90.0 }),
            2 => spec["panes"][0]["scale"] = json!({ "min": -5.0, "max": 50.0, "log": true }),
            3 => spec["panes"][0]["scale"] = json!({ "min": -1e12, "max": 1e12 }),
            4 => spec["width"] = json!(rng.pick(&[0.0, 1.0, 10.0])),
            _ => spec["panes"][0]["scale"]["invert"] = json!(true),
        }
    }
    for fr in frames.iter_mut() {
        if rng.chance(12) {
            fr["barSpacing"] = json!(rng.pick(&[0.0, 1e-9, 1e4, -4.0, 0.05]));
        }
        if rng.chance(10) {
            fr["rightOffset"] = json!(rng.pick(&[1e9, -1e9, -1000.0]));
        }
        if rng.chance(8) {
            fr["top"] = json!(rng.pick(&[-50.0, 1e6]));
            fr["boundsHeight"] = json!(rng.pick(&[-1.0, 0.001, 1e7]));
        }
    }
    spec["frames"] = Value::Array(frames);
    spec
}

// Direct painters (paint_volume / paint_vpvr) with hand-made geometry.

fn run_paint_volume(spec: &Value, datasets: &[Vec<Ohlcv>]) -> Value {
    let bars = &datasets[spec["dataset"].as_u64().unwrap() as usize];
    let g = &spec["geom"];
    let canvas = recording();
    let spacing = num(g, "spacing");
    let right = num(g, "right");
    let x_of = |i: usize| right - (bars.len() as f64 - 1.0 - i as f64) * spacing;
    paint_volume(
        &*canvas,
        bars,
        &VolumeGeom {
            i0: g["i0"].as_u64().unwrap() as usize,
            i1: g["i1"].as_u64().unwrap() as usize,
            x_of: &x_of,
            half_w: num(g, "halfW"),
            bottom_y: num(g, "bottomY"),
            max_h: num(g, "maxH"),
            max_vol: num(g, "maxVol"),
        },
        &VolumeColors {
            up: &string(g, "up"),
            down: &string(g, "down"),
        },
    );
    json!({ "spec": spec, "ops": canvas.to_json() })
}

fn run_paint_vpvr(spec: &Value) -> Value {
    let p = &spec["profile"];
    let profile = VpvrProfile {
        rows: items(p, "rows")
            .iter()
            .map(|r| VpvrRow {
                price: num(r, "price"),
                up: num(r, "up"),
                down: num(r, "down"),
            })
            .collect(),
        row_h: num(p, "rowH"),
        min: num(p, "min"),
        max_total: num(p, "maxTotal"),
        poc: p["poc"].as_u64().unwrap() as usize,
        va_from: p["vaFrom"].as_u64().unwrap() as usize,
        va_to: p["vaTo"].as_u64().unwrap() as usize,
    };
    let g = &spec["geom"];
    let scale = scale_from_json(&g["scale"]);
    let bounds = PaneBounds {
        top: num(g, "top"),
        height: num(g, "height"),
    };
    let coords = CoordinateSystem::new();
    let y_of = |price: f64| coords.price_to_y(price, &scale, &bounds);
    let canvas = recording();
    paint_vpvr(
        &*canvas,
        &profile,
        &VpvrGeom {
            right_x: num(g, "rightX"),
            max_w: num(g, "maxW"),
            y_of: &y_of,
        },
        &VpvrPaintStyle {
            up_color: &string(g, "up"),
            down_color: &string(g, "down"),
            show_poc: flag(g, "showPoc", true),
            poc_color: &string(g, "poc"),
        },
    );
    json!({ "spec": spec, "ops": canvas.to_json() })
}

fn gen_paint_volume(rng: &mut Rng, datasets: &[Value]) -> Value {
    let ds = rng.pick(&[DS_MIN1, DS_QUIRKS, DS_UNEVEN, DS_THREE, DS_SINGLE]);
    let n = datasets[ds]["bars"].as_array().unwrap().len();
    let i0 = rng.below(n as u64);
    let i1 = (i0 + rng.below(40)).min(n - 1);
    let max_vol = rng.pick(&[1000.0, 5_000_000.0, 0.0, -1.0, 1.0, 950.0]);
    json!({
        "dataset": ds,
        "geom": {
            "i0": i0, "i1": if rng.chance(8) { n + 3 } else { i1 }, "spacing": rng.pick(&[0.5, 3.0, 8.0, 12.5]), "right": rng.pick(&[100.0, 640.0]),
            "halfW": rng.pick(&[0.5, 1.0, 3.0, 4.5, 0.0]), "bottomY": rng.pick(&[300.0, 520.5, 100.0]),
            "maxH": rng.pick(&[60.0, 0.0, 200.0, 0.4, -5.0]), "maxVol": max_vol,
            "up": rng.pick(&["#26a69a", "rgba(0,255,0,0.5)"]), "down": rng.pick(&["#ef5350", "#f00"]),
        },
    })
}

fn gen_paint_vpvr(rng: &mut Rng) -> Value {
    let rows_n = rng.pick(&[1usize, 2, 5, 24, 60]);
    let row_h = rng.pick(&[0.5, 1.0, 0.25, 2.0, 0.01]);
    let min = rng.pick(&[100.0, 0.0, 99.5, 10.0]);
    let mut rows = Vec::new();
    let mut max_total = 0.0f64;
    let mut poc = 0;
    for k in 0..rows_n {
        let up = if rng.chance(10) {
            0.0
        } else {
            (rng.below(500) as f64) * 10.0
        };
        let down = if rng.chance(10) {
            0.0
        } else {
            (rng.below(500) as f64) * 10.0
        };
        if up + down > max_total {
            max_total = up + down;
            poc = k;
        }
        rows.push(json!({ "price": min + k as f64 * row_h, "up": up, "down": down }));
    }
    let va_from = rng.below(rows_n as u64).min(poc);
    let va_to = (poc + rng.below(3)).min(rows_n - 1);
    let top = rng.pick(&[0.0, 50.0]);
    let height = rng.pick(&[400.0, 300.5, 120.0]);
    let span = row_h * rows_n as f64;
    let mut scale = json!({ "min": min - span * 0.1, "max": min + span * 1.1 });
    if rng.chance(15) {
        scale["invert"] = json!(true);
    }
    json!({
        "profile": { "rows": rows, "rowH": row_h, "min": min, "maxTotal": if rng.chance(5) { 0.0 } else { max_total }, "poc": poc, "vaFrom": va_from, "vaTo": va_to },
        "geom": {
            "scale": scale, "top": top, "height": height, "rightX": rng.pick(&[640.0, 900.0]), "maxW": rng.pick(&[200.0, 0.0, 50.0, 400.0]),
            "up": rng.pick(&["#2962ff", "rgba(0,0,255,0.5)"]), "down": rng.pick(&["#f23645", "#f00"]), "poc": rng.pick(&["#d1d4dc", "#000"]),
            "showPoc": rng.chance(70),
        },
    })
}

// ───────────────────────────── degenerate inputs ─────────────────────────────

/// Mangles a generated frame the way a corrupt or extreme host state would: flat, inverted,
/// huge or log-unusable price windows, empty panes, extreme zoom and offsets, a missing
/// price pane, indicators on unknown panes, unreadable colors.
fn chaos(rng: &mut Rng, spec: &mut Value) {
    for _ in 0..1 + rng.below(3) {
        let pane_count = spec["panes"].as_array().unwrap().len();
        let k = rng.below(pane_count as u64);
        match rng.below(16) {
            0 => {
                let v = rng.pick(&[100.0, 0.0, -3.5]);
                spec["panes"][k]["scale"] = json!({ "min": v, "max": v });
            }
            1 => spec["panes"][k]["scale"] = json!({ "min": 150.0, "max": 90.0 }),
            2 => spec["panes"][k]["scale"] = json!({ "min": -5.0, "max": 50.0, "log": true }),
            3 => spec["panes"][k]["scale"] = json!({ "min": -1e12, "max": 1e12 }),
            4 => spec["panes"][k]["scale"] = json!({ "min": 1e-9, "max": 2e-9 }),
            5 => spec["panes"][k]["height"] = json!(rng.pick(&[0.0, -5.0, 3.0])),
            6 => {
                spec["viewport"]["barSpacing"] =
                    json!(rng.pick(&[0.0, 0.001, 10_000.0, -4.0, 1e-9]))
            }
            7 => spec["viewport"]["rightOffset"] = json!(rng.pick(&[1e6, -1e6, -1000.0, 1e9])),
            8 => spec["width"] = json!(rng.pick(&[0.0, 1.0, 10.0, 64.0])),
            9 => {
                let panes = spec["panes"].as_array_mut().unwrap();
                if panes.len() > 1 {
                    panes.remove(0);
                    for (i, p) in panes.iter_mut().enumerate() {
                        p["order"] = json!(i);
                    }
                }
            }
            10 => spec["panes"][0]["collapsed"] = json!(true),
            11 => {
                spec["scene"]["style"]["borderColor"] = json!("bogus");
                spec["scene"]["style"]["crosshair"] =
                    json!({ "labelBackground": "not a color", "color": "bogus" });
                spec["scene"]["style"]["gridHorz"] = json!({ "visible": true, "color": "bogus" });
                spec["surface"] = json!({ "background": "bogus", "textColor": "" });
            }
            12 => {
                if let Some(m) = spec["indicators"]
                    .as_array_mut()
                    .and_then(|a| a.first_mut())
                {
                    m["paneId"] = json!("ghost");
                }
            }
            13 => {
                for p in spec["panes"].as_array_mut().unwrap() {
                    p["order"] = json!(0);
                }
            }
            14 => spec["height"] = json!(rng.pick(&[0.0, 1.0, 20.0])),
            _ => {
                spec["scene"]["priceMintick"] = json!(rng.pick(&[0.0, -0.01, 1e-12, 1e9]));
                spec["scene"]["percentBaseline"] = json!(0.0);
                spec["panes"][0]["percentBaseline"] = json!(rng.pick(&[0.0, -50.0, 1e-9]));
            }
        }
    }
}

// ───────────────────────────── main ─────────────────────────────

fn main() {
    // An optional first argument reseeds the generator (the default seed is what the checked-in numbers use).
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(20_251_113);
    let mut rng = Rng(seed.max(1) % 2_147_483_647);
    let ds_values = datasets(&mut rng);
    let ds: Vec<Vec<Ohlcv>> = ds_values
        .iter()
        .map(|d| {
            d["bars"]
                .as_array()
                .unwrap()
                .iter()
                .map(bar_from_json)
                .collect()
        })
        .collect();
    let themes = themes();
    let mut frames = Vec::new();
    let mut n = 0;
    // The geometry backend: candle styles at every zoom, series of every kind, panes and slices.
    for k in 0..130 {
        let dataset = if k % 13 == 0 {
            Some(DS_DENSE)
        } else if k % 29 == 5 {
            Some(DS_THREE)
        } else if k % 31 == 7 {
            Some(DS_SINGLE)
        } else if k % 37 == 9 {
            Some(DS_EMPTY)
        } else {
            None
        };
        let spec = gen_frame(&mut rng, &ds_values, Focus::Backend, dataset, k);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // Every price style at every level-of-detail tier, with per-bar color overrides.
    for k in 0..144 {
        let spec = lod_frame(&mut rng, &ds_values, k);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // The backdrop: grid toggles, session washes, highlights.
    for k in 0..50 {
        let dataset = if k % 17 == 3 { Some(DS_DENSE) } else { None };
        let spec = gen_frame(&mut rng, &ds_values, Focus::Backdrop, dataset, k);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // The chrome: axes, scale modes, tags, countdown, separators, trades, marks.
    for k in 0..130 {
        let dataset = if k % 23 == 4 {
            Some(DS_EMPTY)
        } else if k % 19 == 2 {
            Some(DS_SINGLE)
        } else if k % 21 == 6 {
            Some(DS_THREE)
        } else {
            None
        };
        let spec = gen_frame(&mut rng, &ds_values, Focus::Chrome, dataset, k);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // The crosshair.
    for k in 0..90 {
        let spec = gen_frame(&mut rng, &ds_values, Focus::Crosshair, None, k);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // Pinned corner cases.
    for spec in edge_frames(&mut rng, &ds_values) {
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    // Degenerate and extreme inputs through every painter.
    for k in 0..110 {
        let mut spec = gen_frame(
            &mut rng,
            &ds_values,
            if k % 2 == 0 {
                Focus::Chrome
            } else {
                Focus::Backend
            },
            None,
            k,
        );
        chaos(&mut rng, &mut spec);
        spec["chaos"] = json!(true);
        frames.push(run_frame(&spec, &ds, &themes));
        n += 1;
    }
    let _ = n;

    let marks: Vec<Value> = (0..70)
        .map(|_| run_marks(&gen_marks_scene(&mut rng, &ds_values), &ds))
        .collect();

    let mut volume: Vec<Value> = (0..70)
        .map(|k| {
            run_layer(
                &gen_layer_spec(&mut rng, &ds_values, false, k),
                &ds,
                &themes,
                false,
            )
        })
        .collect();
    let paint_volume_direct: Vec<Value> = (0..40)
        .map(|_| run_paint_volume(&gen_paint_volume(&mut rng, &ds_values), &ds))
        .collect();
    let mut vpvr: Vec<Value> = (0..70)
        .map(|k| {
            run_layer(
                &gen_layer_spec(&mut rng, &ds_values, true, k),
                &ds,
                &themes,
                true,
            )
        })
        .collect();
    let paint_vpvr_direct: Vec<Value> = (0..40)
        .map(|_| run_paint_vpvr(&gen_paint_vpvr(&mut rng)))
        .collect();
    // The layer frames a zero-spacing viewport makes NaN-ranged (the original's VPVR layer reads bars[NaN] there).
    let mut layer_edges = Vec::new();
    for as_vpvr in [false, true] {
        for k in 0..2 {
            let mut spec = gen_layer_spec(&mut rng, &ds_values, as_vpvr, k);
            for fr in spec["frames"].as_array_mut().unwrap() {
                fr["barSpacing"] = json!(0.0);
                fr["visible"] = json!(true);
                fr.as_object_mut().unwrap().remove("noData");
                fr["boundsHeight"] = json!(300.0);
            }
            spec["edge"] = json!("nan-visible-range");
            layer_edges.push((as_vpvr, run_layer(&spec, &ds, &themes, as_vpvr)));
        }
    }
    for (is_vpvr, result) in layer_edges {
        if is_vpvr {
            vpvr.push(result);
        } else {
            volume.push(result);
        }
    }

    let out = json!({
        "datasets": ds_values,
        "themes": themes,
        "frames": frames,
        "marks": marks,
        "volume": volume,
        "paintVolume": paint_volume_direct,
        "vpvr": vpvr,
        "paintVpvr": paint_vpvr_direct,
    });
    println!("{out}");
}
