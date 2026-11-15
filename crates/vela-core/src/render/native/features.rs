//! Render features and the serializable config: `applyFeature` / `readFeature`,
//! `getConfig` / `applyConfig` (the part that does not touch the price style,
//! which the wrapper runs between its two phases), the theme.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::marks::MarksDisplayState;
use crate::options::{
    AUTOSCALE_EASE_DEFAULT_MS, FreePanTrigger, INTRO_DURATION_DEFAULT_MS, IntroAnimation,
    PAN_INERTIA_DEFAULT_MS, SCROLL_EASE_DEFAULT_MS, ZOOM_EASE_DEFAULT_MS, resolve_ease_ms,
    resolve_intro, resolve_live_bar_ease_ms,
};
use crate::render::core::chart_config::{
    AnimationsConfig, AreaConfig, AreaSeriesStyle, BASELINE_BOTTOM_LINE, BASELINE_FILL_ALPHA,
    BASELINE_FILL_ALPHA_FAR, BASELINE_TOP_LINE, BarsConfig, BarsStyle, BaselineConfig,
    BaselineSeriesStyle, CHART_CONFIG_VERSION, CandleColors, CandleStyle, CandlesConfig,
    ChartConfig, ColumnsConfig, ColumnsSeriesStyle, CrosshairConfig, CrosshairStyle, GridConfig,
    GridLineConfig, GridLineStyle, LayoutConfig, LineConfig, LineSeriesStyle, MarksConfig,
    PanesConfig, PriceScaleConfig, SeriesConfig, SessionShadeStyle, SessionsConfig, StackingConfig,
    TRANSPARENT, TimeScaleConfig, TradesConfig, candle_override_for, columns_color_follower,
    columns_default_colors, merge_config, sanitize_crosshair_override, theme_color_follower,
};
use crate::render::core::color::with_alpha;
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::shared::marks_state::merge_marks_state;
use crate::render::shared::trade_markers::{
    TradeMarkerColors, TradeMarkersState, merge_trade_markers_state,
};
use crate::theme::{dark_theme, light_theme};

use super::state::{Event, PRICE_PANE_ID, State, derive_theme};
use super::util::{
    as_scale_mode, is_dark_color, js_number_or_zero, js_to_number, js_to_string, js_truthy,
    sanitize_highlights, sanitize_session_zones, theme_string,
};

/// The feature keys the native renderer reads and writes at runtime.
pub(crate) const FEATURES: &[&str] = &[
    "logScale",
    "currentPriceLine",
    "priceLabel",
    "countdown",
    "upColor",
    "downColor",
    "glow",
    "animZoom",
    "animPan",
    "animScroll",
    "animAutoscale",
    "animLiveBar",
    "intro",
    "zoomAnchor",
    "axisDrag",
    "paneResize",
    "freePan",
    "candleZOrder",
    "candleVisible",
    "seriesOrder",
    "highlights",
    "sessionZones",
    "gridlines",
    "axisLabels",
    "scaleMode",
    "invertScale",
    "scalePriceOnly",
    "paneScales",
    "autoScale",
    "timezone",
    "keyboard",
    "historyChords",
    "priceStyle",
    "shownPriceStyle",
    "priceBaseline",
    "baselinePrice",
    "previousClose",
    "settings",
    "attribution",
    "dialogHost",
    "tradeMarkers",
    "marks",
    "indicatorTitles",
    "indicatorValues",
    "crosshairOverride",
    "drawingsInteractive",
    "priceAxisTicks",
];

/// A number as JSON the way JavaScript would print it: whole values as integers.
pub(crate) fn json_num(v: f64) -> Value {
    if !v.is_finite() {
        Value::Null
    } else if v.fract() == 0.0 && v.abs() < 9.0e15 {
        Value::from(v as i64)
    } else {
        Value::from(v)
    }
}

fn pane_scope(value: &Value, key: &str) -> Option<(String, Value)> {
    let obj = value.as_object()?;
    let pane = obj
        .get("pane")
        .and_then(Value::as_str)
        .unwrap_or(PRICE_PANE_ID)
        .to_string();
    Some((pane, obj.get(key).cloned().unwrap_or(Value::Null)))
}

/// `keepInherit`: keep the "inherit the theme" sentinel (`None`) whenever the merged
/// value is just the resolved echo of it.
fn keep_inherit(cur: &Option<String>, merged: &str, resolved: &str) -> Option<String> {
    if cur.is_none() && merged == resolved {
        None
    } else {
        Some(merged.to_string())
    }
}

impl State {
    /// Apply a render feature live: mutate the field and invalidate, no engine re-run.
    /// `priceStyle` is the wrapper's (it announces the change to subscribers).
    pub(crate) fn apply_feature(&mut self, key: &str, value: &Value) {
        match key {
            "logScale" => {
                // `{ pane, value }` targets one pane; a bare boolean is the price pane.
                if value.is_object() {
                    if let Some((pane, v)) = pane_scope(value, "value") {
                        self.set_pane_log(&pane, js_truthy(&v));
                    }
                    return; // set_pane_log invalidates
                }
                self.scene.log_scale = js_truthy(value);
            }
            "currentPriceLine" => self.scene.show_price_line = js_truthy(value),
            "priceLabel" => self.scene.show_price_label = js_truthy(value),
            "countdown" => {
                self.scene.show_countdown = js_truthy(value);
                self.sync_countdown_timer();
            }
            "upColor" => {
                self.candle_up = js_to_string(value);
                self.theme = derive_theme(&self.theme, &self.candle_up, &self.candle_down);
            }
            "downColor" => {
                self.candle_down = js_to_string(value);
                self.theme = derive_theme(&self.theme, &self.candle_up, &self.candle_down);
            }
            // The WebGL2 neon glow: canvas2d ignores it.
            "glow" => {
                self.glow_amount = js_number_or_zero(value);
                self.backend.set_glow(self.glow_amount);
            }
            "animZoom" => {
                self.anim_zoom
                    .set(resolve_ease_ms(value, ZOOM_EASE_DEFAULT_MS));
                return; // affects the next interaction only: nothing to repaint
            }
            "animPan" => {
                // `animPan` is the umbrella for pan motion: it also switches the
                // programmatic scroll glide on/off; `animScroll` tunes that one alone.
                let ms = resolve_ease_ms(value, PAN_INERTIA_DEFAULT_MS);
                self.anim_pan.set(ms);
                self.anim_scroll.toggle(ms > 0.0);
                return;
            }
            "animScroll" => {
                self.anim_scroll
                    .set(resolve_ease_ms(value, SCROLL_EASE_DEFAULT_MS));
                return;
            }
            "animAutoscale" => {
                self.anim_autoscale
                    .set(resolve_ease_ms(value, AUTOSCALE_EASE_DEFAULT_MS));
                return; // a glide in flight finishes at the new rate (or snaps at 0)
            }
            "animLiveBar" => {
                self.anim_live_bar.set(resolve_live_bar_ease_ms(value));
                return;
            }
            "intro" => {
                self.set_intro(resolve_intro(value));
                if self.intro.style.is_some() && self.mounted {
                    self.play_intro(); // setting it replays: compare styles from the console
                }
                return;
            }
            "zoomAnchor" => {
                self.zoom_anchor_right = value.as_str() == Some("right");
                return;
            }
            "axisDrag" => {
                self.axis_drag_enabled = js_truthy(value);
                return;
            }
            "paneResize" => {
                self.pane_resize_enabled = js_truthy(value);
                return;
            }
            "freePan" => {
                self.apply_free_pan(value);
                return;
            }
            "candleZOrder" => self.scene.candle_z = js_number_or_zero(value),
            "candleVisible" => self.scene.candles_hidden = value == &Value::Bool(false),
            "seriesOrder" => self.apply_series_order(value),
            "highlights" => self.scene.highlights = sanitize_highlights(value),
            // Session bands from the host's market calendar. Null: no session structure.
            "sessionZones" => self.scene.session_zones = sanitize_session_zones(value),
            "gridlines" => self.scene.show_grid = js_truthy(value),
            "axisLabels" => self.scene.show_axis_labels = js_truthy(value),
            "scaleMode" => {
                // `{ pane, mode }` targets one pane; a bare string is the price pane.
                if value.is_object() {
                    if let Some((pane, v)) = pane_scope(value, "mode") {
                        self.set_pane_scale_mode(&pane, as_scale_mode(&v));
                    }
                    return;
                }
                self.scene.scale_mode = as_scale_mode(value);
            }
            "invertScale" => {
                if value.is_object() {
                    if let Some((pane, v)) = pane_scope(value, "value") {
                        self.set_pane_invert(&pane, js_truthy(&v));
                    }
                    return;
                }
                self.scene.invert_scale = js_truthy(value);
            }
            "scalePriceOnly" => self.scene.scale_price_only = js_truthy(value),
            "autoScale" => {
                // Auto on: drop every pane's frozen window so autoscale resumes. Auto
                // off: freeze each pane's current window into manual mode.
                let auto = js_truthy(value);
                for pane in self.scene.panes.values_mut() {
                    pane.manual_scale = if auto { None } else { Some(pane.scale) };
                }
            }
            "timezone" => {
                self.scene.timezone = match value.as_str() {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => "UTC".to_string(),
                };
            }
            "priceBaseline" => {
                self.scene.baseline_value = if value.is_null() {
                    None
                } else {
                    Some(js_to_number(value))
                };
            }
            "tradeMarkers" => {
                // Partial state merge: malformed fields drop.
                self.scene.trade_markers =
                    merge_trade_markers_state(&self.scene.trade_markers, value);
            }
            "marks" => {
                // `false`/`true` toggles the lane; `{ visible?, groups? }` merges partially.
                self.scene.marks = merge_marks_state(&self.scene.marks, value);
                self.close_mark_popover(); // the open cluster may just have been hidden
            }
            "crosshairOverride" => {
                // Runtime-only (never in the config): a pick interaction restyles the
                // cursor and a reload can never leave it stuck. Null restores it.
                self.scene.crosshair_override = sanitize_crosshair_override(value);
            }
            "drawingsInteractive" => {
                // Runtime-only, like `crosshairOverride`: a pick takes the plot's
                // presses for itself.
                self.drawings_interactive = value != &Value::Bool(false);
                if !self.drawings_interactive
                    && let Some(h) = &self.user_drawings
                {
                    h.clear_hover();
                }
                return;
            }
            "priceAxisTicks" => {
                // The hook is a function (see `NativeRenderer::set_price_axis_ticks`).
                // Through a value, `null`/`false` clears it; anything else only drops
                // the memoized ticks so a host can refresh them.
                if value.is_null() || value == &Value::Bool(false) {
                    self.price_axis_ticks.set_hook(None);
                } else {
                    self.price_axis_ticks.invalidate();
                }
            }
            "keyboard" => {
                self.keyboard_enabled = js_truthy(value);
                self.host.set_keyboard_enabled(self.keyboard_enabled);
                return; // owns its own DOM (focus, listeners, live region)
            }
            "historyChords" => {
                // Off: the host keymap owns Ctrl+Z/Y; the drawings layer stops consuming
                // the chords so they bubble up.
                self.history_chords_enabled = js_truthy(value);
                if let Some(h) = &self.user_drawings {
                    h.set_history_chords(self.history_chords_enabled);
                }
                return;
            }
            "settings" => {
                self.settings_enabled = js_truthy(value);
                self.host.feature_changed(key, value);
                return; // owns its own DOM (gear button + dialog)
            }
            "indicatorTitles" => {
                self.indicator_titles_on = value != &Value::Bool(false);
                self.host.feature_changed(key, value);
                return;
            }
            "indicatorValues" => {
                self.indicator_values_on = value != &Value::Bool(false);
                self.host.feature_changed(key, value);
                return;
            }
            "attribution" => {
                // `false` hides it, `true` restores the built-in mark, a non-empty STRING
                // puts the host's own mark in that corner.
                self.attribution_html = match value.as_str() {
                    Some(s) if !s.trim().is_empty() => Some(s.to_string()),
                    _ => None,
                };
                self.attribution_enabled = self.attribution_html.is_some() || js_truthy(value);
                self.attribution_sent = None;
                self.position_attribution();
                return;
            }
            "dialogHost" => {
                // Where MODAL dialogs mount: a DOM element, so the value is the host's.
                self.dialog_host_set = !value.is_null();
                self.host.feature_changed(key, value);
                return;
            }
            _ => return,
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// The `freePan` feature: `true`/`'drag'`/`'alt'`/`'shift'`/`'mod'` turn it on with
    /// that trigger; anything else is a plain on/off.
    fn apply_free_pan(&mut self, value: &Value) {
        let trigger = match value {
            Value::Bool(true) => Some(FreePanTrigger::Drag),
            Value::String(s) => match s.as_str() {
                "drag" => Some(FreePanTrigger::Drag),
                "alt" => Some(FreePanTrigger::Alt),
                "shift" => Some(FreePanTrigger::Shift),
                "mod" => Some(FreePanTrigger::Mod),
                _ => None,
            },
            _ => None,
        };
        match trigger {
            Some(t) => self.apply_free_pan_trigger(t),
            None => self.free_pan_on = js_truthy(value),
        }
    }

    /// Turn free panning on with a (possibly custom) trigger, or off with `Enabled(false)`.
    pub(crate) fn apply_free_pan_trigger(&mut self, trigger: FreePanTrigger) {
        match trigger {
            FreePanTrigger::Enabled(true) => {
                self.free_pan_trigger = FreePanTrigger::Drag;
                self.free_pan_on = true;
            }
            FreePanTrigger::Enabled(false) => self.free_pan_on = false,
            other => {
                self.free_pan_trigger = other;
                self.free_pan_on = true;
            }
        }
    }

    /// Reorder one indicator's series layer. Accepts `{ id, to: 'front' | 'back' }`
    /// (relative to every layer incl. the candles) or `{ id, z }` (an explicit key).
    fn apply_series_order(&mut self, value: &Value) {
        let Some(v) = value.as_object() else { return };
        let Some(id) = v.get("id").and_then(Value::as_str) else {
            return;
        };
        if let Some(z) = v.get("z").and_then(Value::as_f64) {
            self.scene.set_indicator_z(id, z);
        } else {
            match v.get("to").and_then(Value::as_str) {
                Some("front") => self.scene.bring_indicator_to_front(id),
                Some("back") => self.scene.send_indicator_to_back(id),
                _ => {}
            }
        }
        self.sync_legend_order();
    }

    pub(crate) fn read_feature(&self, key: &str) -> Option<Value> {
        let scene = &self.scene;
        Some(match key {
            "logScale" => json!(scene.log_scale),
            "currentPriceLine" => json!(scene.show_price_line),
            "priceLabel" => json!(scene.show_price_label),
            "countdown" => json!(scene.show_countdown),
            "upColor" => json!(self.candle_up),
            "downColor" => json!(self.candle_down),
            "glow" => json_num(self.glow_amount),
            "animZoom" => json_num(self.anim_zoom.tau()),
            "animPan" => json_num(self.anim_pan.tau()),
            "animScroll" => json_num(self.anim_scroll.tau()),
            "animAutoscale" => json_num(self.anim_autoscale.tau()),
            "animLiveBar" => json_num(self.anim_live_bar.tau()),
            "intro" => match self.intro.style {
                Some(s) => json!(s.as_str()),
                None => json!(false),
            },
            "zoomAnchor" => json!(if self.zoom_anchor_right { "right" } else { "cursor" }),
            "axisDrag" => json!(self.axis_drag_enabled),
            "paneResize" => json!(self.pane_resize_enabled),
            "freePan" => {
                if !self.free_pan_on {
                    json!(false)
                } else {
                    match &self.free_pan_trigger {
                        FreePanTrigger::Alt => json!("alt"),
                        FreePanTrigger::Shift => json!("shift"),
                        FreePanTrigger::Mod => json!("mod"),
                        _ => json!(true),
                    }
                }
            }
            "candleZOrder" => json_num(scene.candle_z),
            "candleVisible" => json!(!scene.candles_hidden),
            "seriesOrder" => Value::Array(
                scene
                    .indicator_z_order()
                    .into_iter()
                    .map(|e| json!({ "id": e.id, "z": json_num(e.z) }))
                    .collect(),
            ),
            "highlights" => Value::Array(
                scene
                    .highlights
                    .iter()
                    .map(|h| json!({ "from": h.from, "to": h.to, "color": h.color }))
                    .collect(),
            ),
            "sessionZones" => match &scene.session_zones {
                None => Value::Null,
                Some(z) => {
                    let windows = |w: &[(i64, i64)]| {
                        Value::Array(w.iter().map(|(a, b)| json!([a, b])).collect())
                    };
                    json!({
                        "pre": windows(&z.pre),
                        "post": windows(&z.post),
                        "extended": windows(&z.extended),
                    })
                }
            },
            "gridlines" => json!(scene.show_grid),
            "axisLabels" => json!(scene.show_axis_labels),
            "scaleMode" => json!(scene.scale_mode.as_str()),
            "invertScale" => json!(scene.invert_scale),
            "scalePriceOnly" => json!(scene.scale_price_only),
            "paneScales" => Value::Array(
                self.pane_scale_infos()
                    .into_iter()
                    .map(|(id, kind, top, height, mode, log, invert)| {
                        json!({
                            "id": id,
                            "kind": if kind == crate::model::PaneKind::Price { "price" } else { "study" },
                            "top": json_num(top),
                            "height": json_num(height),
                            "mode": mode.as_str(),
                            "log": log,
                            "invert": invert,
                        })
                    })
                    .collect(),
            ),
            "autoScale" => {
                // Auto when the price pane isn't holding a frozen (manual) window.
                let price = scene
                    .ordered_panes()
                    .into_iter()
                    .find(|p| p.kind == crate::model::PaneKind::Price);
                json!(price.is_none_or(|p| p.manual_scale.is_none()))
            }
            "timezone" => json!(scene.timezone),
            "priceStyle" => json!(self.chosen_style),
            // READ-ONLY: the style the chart DRAWS.
            "shownPriceStyle" => json!(scene.price_style),
            "priceBaseline" => scene.baseline_value.map_or(Value::Null, json_num),
            "baselinePrice" => {
                // READ-ONLY: the RESOLVED baseline reference price the paint splits on.
                match scene.panes.get(PRICE_PANE_ID) {
                    Some(p) => json_num(scene.baseline_price_for(&p.scale)),
                    None => scene.baseline_value.map_or(Value::Null, json_num),
                }
            }
            "previousClose" => {
                // READ-ONLY: the close of the bar before the one the readouts show.
                let idx = self.hover_logical.unwrap_or(self.bars.len() as i64 - 1);
                match usize::try_from(idx - 1).ok().and_then(|i| self.bars.get(i)) {
                    Some(b) if b.close.is_finite() => json_num(b.close),
                    _ => Value::Null,
                }
            }
            "tradeMarkers" => json!({
                "visible": scene.trade_markers.visible,
                "labels": scene.trade_markers.labels,
                "qty": scene.trade_markers.qty,
                "colors": {
                    "long": scene.trade_markers.colors.long,
                    "short": scene.trade_markers.colors.short,
                    "exit": scene.trade_markers.colors.exit,
                },
            }),
            "marks" => json!({
                "visible": scene.marks.visible,
                "groups": scene.marks.groups,
            }),
            "crosshairOverride" => match &scene.crosshair_override {
                Some(o) => serde_json::to_value(o).unwrap_or(Value::Null),
                None => Value::Null,
            },
            "drawingsInteractive" => json!(self.drawings_interactive),
            "priceAxisTicks" => {
                if self.price_axis_ticks.hook().is_some() {
                    json!(true)
                } else {
                    Value::Null
                }
            }
            "keyboard" => json!(self.keyboard_enabled),
            "historyChords" => json!(self.history_chords_enabled),
            "settings" => json!(self.settings_enabled),
            "indicatorTitles" => json!(self.indicator_titles_on),
            "indicatorValues" => json!(self.indicator_values_on),
            "attribution" => match &self.attribution_html {
                Some(h) => json!(h),
                None => json!(self.attribution_enabled),
            },
            _ => return None,
        })
    }

    // ── the serializable config ──

    /// Snapshot the full chart cosmetics as one serializable, versioned document.
    /// Every inherited value is resolved to a concrete one so an exported template
    /// stands on its own.
    pub(crate) fn get_config(&self) -> ChartConfig {
        let s = &self.scene.style;
        let t = &self.theme;
        let grid_color = theme_string(t, "gridColor");
        let text_color = theme_string(t, "textColor");
        let border_color = theme_string(t, "borderColor");
        let up = &self.candle_up;
        let down = &self.candle_down;
        let tm = &self.scene.trade_markers;
        let line_color = s.area.line_color.clone().unwrap_or_else(|| up.clone());
        let top_line = s
            .baseline
            .top_line_color
            .clone()
            .unwrap_or_else(|| BASELINE_TOP_LINE.to_string());
        let bottom_line = s
            .baseline
            .bottom_line_color
            .clone()
            .unwrap_or_else(|| BASELINE_BOTTOM_LINE.to_string());
        let wash = columns_default_colors(&CandleColors {
            up_color: up.clone(),
            down_color: down.clone(),
        });
        ChartConfig {
            version: CHART_CONFIG_VERSION,
            chart_types: s.chart_types.clone(),
            layout: LayoutConfig {
                background: theme_string(t, "background"),
                text_color: text_color.clone(),
                font_family: theme_string(t, "fontFamily"),
                font_size: s.font_size,
            },
            grid: GridConfig {
                vert_lines: GridLineConfig {
                    visible: s.grid_vert.visible,
                    color: s
                        .grid_vert
                        .color
                        .clone()
                        .unwrap_or_else(|| grid_color.clone()),
                },
                horz_lines: GridLineConfig {
                    visible: s.grid_horz.visible,
                    color: s.grid_horz.color.clone().unwrap_or(grid_color),
                },
            },
            crosshair: CrosshairConfig {
                color: s
                    .crosshair
                    .color
                    .clone()
                    .unwrap_or_else(|| text_color.clone()),
                width: s.crosshair.width,
                style: s.crosshair.style,
                opacity: s.crosshair.opacity,
                label_background: s
                    .crosshair
                    .label_background
                    .clone()
                    .unwrap_or_else(|| text_color.clone()),
            },
            price_scale: PriceScaleConfig {
                mode: self.scene.scale_mode,
                log: self.scene.log_scale,
                invert: self.scene.invert_scale,
                price_only: self.scene.scale_price_only,
                free_pan: self.free_pan_on,
                border_color: s
                    .border_color
                    .clone()
                    .unwrap_or_else(|| border_color.clone()),
                labels_visible: self.scene.show_axis_labels,
                current_price_line: self.scene.show_price_line,
                price_label: self.scene.show_price_label,
                countdown: self.scene.show_countdown,
                animate_last_price: self.anim_live_bar.on(),
            },
            animations: AnimationsConfig {
                zoom: self.anim_zoom.on(),
                pan: self.anim_pan.on(),
                autoscale: self.anim_autoscale.on(),
                intro: self.intro.style.is_some(),
            },
            panes: PanesConfig {
                separator_color: s.separator_color.clone().unwrap_or(border_color),
            },
            margins: s.margins,
            trades: TradesConfig {
                visible: tm.visible,
                labels: tm.labels,
                qty: tm.qty,
                long_color: tm.colors.long.clone(),
                short_color: tm.colors.short.clone(),
                exit_color: tm.colors.exit.clone(),
            },
            time_scale: TimeScaleConfig {
                timezone: self.scene.timezone.clone(),
            },
            marks: MarksConfig {
                visible: self.scene.marks.visible,
                groups: self
                    .scene
                    .marks
                    .groups
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect::<BTreeMap<_, _>>(),
            },
            candles: CandlesConfig {
                up_color: up.clone(),
                down_color: down.clone(),
                body_visible: s.candle.body_visible,
                border_visible: s.candle.border_visible,
                border_up_color: s
                    .candle
                    .border_up_color
                    .clone()
                    .unwrap_or_else(|| up.clone()),
                border_down_color: s
                    .candle
                    .border_down_color
                    .clone()
                    .unwrap_or_else(|| down.clone()),
                wick_visible: s.candle.wick_visible,
                wick_up_color: s.candle.wick_up_color.clone().unwrap_or_else(|| up.clone()),
                wick_down_color: s
                    .candle
                    .wick_down_color
                    .clone()
                    .unwrap_or_else(|| down.clone()),
            },
            bars: BarsConfig {
                up_color: s.bars.up_color.clone().unwrap_or_else(|| up.clone()),
                down_color: s.bars.down_color.clone().unwrap_or_else(|| down.clone()),
            },
            line: LineConfig {
                color: s.line.color.clone().unwrap_or_else(|| up.clone()),
                width: s.line.width,
            },
            stacking: StackingConfig {
                candles: self.scene.candle_z,
                series: self
                    .scene
                    .indicator_z_order()
                    .into_iter()
                    .map(|e| (e.id, e.z))
                    .collect(),
            },
            area: AreaConfig {
                top_color: s
                    .area
                    .top_color
                    .clone()
                    .unwrap_or_else(|| line_color.clone()),
                bottom_color: s
                    .area
                    .bottom_color
                    .clone()
                    .unwrap_or_else(|| TRANSPARENT.to_string()),
                line_color,
                width: s.area.width,
            },
            baseline: BaselineConfig {
                top_fill_color: s
                    .baseline
                    .top_fill_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(&top_line, BASELINE_FILL_ALPHA)),
                top_fill_color2: s
                    .baseline
                    .top_fill_color2
                    .clone()
                    .unwrap_or_else(|| with_alpha(&top_line, BASELINE_FILL_ALPHA_FAR)),
                bottom_fill_color: s
                    .baseline
                    .bottom_fill_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(&bottom_line, BASELINE_FILL_ALPHA)),
                bottom_fill_color2: s
                    .baseline
                    .bottom_fill_color2
                    .clone()
                    .unwrap_or_else(|| with_alpha(&bottom_line, BASELINE_FILL_ALPHA_FAR)),
                top_line_color: top_line,
                bottom_line_color: bottom_line,
                width: s.baseline.width,
                baseline_level: s.baseline.baseline_level,
            },
            columns: ColumnsConfig {
                up_color: s.columns.up_color.clone().unwrap_or(wash.up_color),
                down_color: s.columns.down_color.clone().unwrap_or(wash.down_color),
            },
            series: SeriesConfig {
                style: self.chosen_style.clone(),
                baseline: self.scene.baseline_value,
                spacing: self.coords.spacing_scale(),
            },
            sessions: SessionsConfig {
                premarket_color: s.sessions.premarket_color.clone(),
                postmarket_color: s.sessions.postmarket_color.clone(),
                extended_color: s.sessions.extended_color.clone(),
            },
        }
    }

    /// The first half of `applyConfig`: everything up to the price style. Returns the
    /// merged document; the wrapper then writes the price style (announcing it) and
    /// calls [`apply_config_tail`](Self::apply_config_tail).
    pub(crate) fn apply_config_head(&mut self, config: &Value) -> ChartConfig {
        let next = {
            let registry = self.chart_types.borrow();
            merge_config(&self.get_config(), config, &registry)
        };
        // Chart-type SDK settings: on change, store + push the type's `<id>-settings`
        // channel (its renderer layer reads it) + notify the core (its data engine).
        for (type_id, vals) in &next.chart_types {
            if self.scene.style.chart_types.get(type_id) == Some(vals) {
                continue;
            }
            self.scene
                .native_data
                .insert(format!("{type_id}-settings"), Value::Object(vals.clone()));
            self.emit(Event::ChartTypeSettings(type_id.clone(), vals.clone()));
        }
        self.scene.style.chart_types = next.chart_types.clone();
        // The chrome surface (toolbar/scales) deliberately does NOT follow the config:
        // it is owned by the app theme alone, so background edits recolor only the plot.
        //
        // A background edit that lands the plot in the OTHER luminance class (a dark
        // theme with a white background typed into the settings dialog) would leave the
        // derived inks unreadable. When the resulting text/background pair falls in the
        // SAME luminance class and the patch itself did not choose a text color, re-base
        // the derived inks (text/grid/border) from the built-in theme of the new class.
        // An explicit `layout.textColor` in the same patch always wins.
        let explicit_text_color = config
            .get("layout")
            .and_then(|l| l.get("textColor"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let prev_theme = self.theme.clone();
        let mut inks = (
            next.layout.text_color.clone(),
            theme_string(&prev_theme, "gridColor"),
            theme_string(&prev_theme, "borderColor"),
        );
        if !explicit_text_color
            && is_dark_color(&next.layout.text_color) == is_dark_color(&next.layout.background)
        {
            let rebase = if is_dark_color(&next.layout.background) {
                dark_theme()
            } else {
                light_theme()
            };
            inks = (
                theme_string(&rebase, "textColor"),
                theme_string(&rebase, "gridColor"),
                theme_string(&rebase, "borderColor"),
            );
        }
        if let Some(obj) = self.theme.as_object_mut() {
            obj.insert("background".into(), json!(next.layout.background));
            obj.insert("textColor".into(), json!(inks.0));
            obj.insert("gridColor".into(), json!(inks.1));
            obj.insert("borderColor".into(), json!(inks.2));
            obj.insert("fontFamily".into(), json!(next.layout.font_family));
        }
        let prev_grid = theme_string(&prev_theme, "gridColor");
        let prev_border = theme_string(&prev_theme, "borderColor");
        let s = &mut self.scene.style;
        s.font_size = next.layout.font_size;
        // `merge_config` runs over the RESOLVED get_config(), so a live "inherit the
        // theme" sentinel (None) comes back as its concrete value even when the patch
        // never named the field. Writing that back would pin the old theme's color
        // forever; keep the sentinel whenever the merged value is the resolved echo.
        s.grid_vert = GridLineStyle {
            visible: next.grid.vert_lines.visible,
            color: keep_inherit(&s.grid_vert.color, &next.grid.vert_lines.color, &prev_grid),
        };
        s.grid_horz = GridLineStyle {
            visible: next.grid.horz_lines.visible,
            color: keep_inherit(&s.grid_horz.color, &next.grid.horz_lines.color, &prev_grid),
        };
        s.crosshair = CrosshairStyle {
            color: Some(next.crosshair.color.clone()),
            width: next.crosshair.width,
            style: next.crosshair.style,
            opacity: next.crosshair.opacity,
            label_background: Some(next.crosshair.label_background.clone()),
        };
        self.scene.scale_mode = next.price_scale.mode;
        self.scene.log_scale = next.price_scale.log;
        self.scene.invert_scale = next.price_scale.invert;
        self.scene.scale_price_only = next.price_scale.price_only;
        self.free_pan_on = next.price_scale.free_pan;
        let s = &mut self.scene.style;
        s.border_color = keep_inherit(
            &s.border_color,
            &next.price_scale.border_color,
            &prev_border,
        );
        self.scene.show_axis_labels = next.price_scale.labels_visible;
        self.scene.show_price_line = next.price_scale.current_price_line;
        self.scene.show_price_label = next.price_scale.price_label;
        self.scene.show_countdown = next.price_scale.countdown;
        self.sync_countdown_timer();
        // animations: on/off only; the durations are the host's (EaseSetting remembers them)
        self.anim_live_bar
            .toggle(next.price_scale.animate_last_price);
        self.anim_zoom.toggle(next.animations.zoom);
        self.anim_pan.toggle(next.animations.pan);
        self.anim_scroll.toggle(next.animations.pan); // the scroll glide is pan motion too
        self.anim_autoscale.toggle(next.animations.autoscale);
        self.intro = IntroAnimation {
            style: next.animations.intro.then_some(self.intro_on_style),
            duration: if self.intro.duration == 0.0 || self.intro.duration.is_nan() {
                INTRO_DURATION_DEFAULT_MS
            } else {
                self.intro.duration
            },
        };
        let s = &mut self.scene.style;
        s.separator_color = keep_inherit(
            &s.separator_color,
            &next.panes.separator_color,
            &prev_border,
        );
        // margins: top/bottom flow into the next autoscale pass; a right-margin edit
        // re-lands the newest bar on the new whitespace at the current zoom, so it shows
        // immediately instead of on the next fit.
        if next.margins.right != self.scene.style.margins.right && self.coords.bar_count() > 0 {
            let bar_spacing = self.viewport().bar_spacing;
            self.apply_viewport(crate::render::core::ViewportState {
                bar_spacing,
                right_offset: next.margins.right,
            });
        }
        self.scene.style.margins = next.margins;
        self.scene.trade_markers = TradeMarkersState {
            visible: next.trades.visible,
            labels: next.trades.labels,
            qty: next.trades.qty,
            colors: TradeMarkerColors {
                long: next.trades.long_color.clone(),
                short: next.trades.short_color.clone(),
                exit: next.trades.exit_color.clone(),
            },
        };
        self.scene.timezone = next.time_scale.timezone.clone();
        // timeline marks (lane toggle + per-group visibility)
        self.scene.marks = MarksDisplayState {
            visible: next.marks.visible,
            groups: next
                .marks
                .groups
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect::<HashMap<_, _>>(),
        };
        self.candle_up = next.candles.up_color.clone();
        self.candle_down = next.candles.down_color.clone();
        let s = &mut self.scene.style;
        s.candle = CandleStyle {
            body_visible: next.candles.body_visible,
            border_visible: next.candles.border_visible,
            border_up_color: Some(next.candles.border_up_color.clone()),
            border_down_color: Some(next.candles.border_down_color.clone()),
            wick_visible: next.candles.wick_visible,
            wick_up_color: Some(next.candles.wick_up_color.clone()),
            wick_down_color: Some(next.candles.wick_down_color.clone()),
        };
        // stacking: the candles' draw-order key plus each indicator's. Seeding an id
        // BEFORE its indicator mounts also works: assign_indicator_z keeps an existing
        // key, so a restored stack survives the (async) indicator restore.
        self.scene.candle_z = next.stacking.candles;
        for (id, z) in &next.stacking.series {
            self.scene.set_indicator_z(id, *z);
        }
        self.sync_legend_order();
        // per-price-style cosmetics (independent of the candle palette)
        let s = &mut self.scene.style;
        s.bars = BarsStyle {
            up_color: Some(next.bars.up_color.clone()),
            down_color: Some(next.bars.down_color.clone()),
        };
        s.line = LineSeriesStyle {
            color: Some(next.line.color.clone()),
            width: next.line.width,
        };
        s.area = AreaSeriesStyle {
            line_color: Some(next.area.line_color.clone()),
            width: next.area.width,
            top_color: Some(next.area.top_color.clone()),
            bottom_color: Some(next.area.bottom_color.clone()),
        };
        s.baseline = BaselineSeriesStyle {
            top_line_color: Some(next.baseline.top_line_color.clone()),
            bottom_line_color: Some(next.baseline.bottom_line_color.clone()),
            top_fill_color: Some(next.baseline.top_fill_color.clone()),
            top_fill_color2: Some(next.baseline.top_fill_color2.clone()),
            bottom_fill_color: Some(next.baseline.bottom_fill_color.clone()),
            bottom_fill_color2: Some(next.baseline.bottom_fill_color2.clone()),
            width: next.baseline.width,
            baseline_level: next.baseline.baseline_level,
        };
        s.columns = ColumnsSeriesStyle {
            up_color: Some(next.columns.up_color.clone()),
            down_color: Some(next.columns.down_color.clone()),
        };
        // session shading
        s.sessions = SessionShadeStyle {
            premarket_color: next.sessions.premarket_color.clone(),
            postmarket_color: next.sessions.postmarket_color.clone(),
            extended_color: next.sessions.extended_color.clone(),
        };
        next
    }

    /// The second half of `applyConfig`, after the price style was written.
    pub(crate) fn apply_config_tail(&mut self, next: &ChartConfig) {
        // Re-read the active style's candle override: set_price_style no-ops when the
        // style did not change, but the per-type bag (candle* keys) may have: this is
        // the live path behind the Symbol tab's Candles group for plugin styles.
        {
            let registry = self.chart_types.borrow();
            self.scene.candle_override = candle_override_for(
                &self.scene.price_style,
                &self.scene.style.chart_types,
                &registry,
            );
        }
        self.scene.baseline_value = next.series.baseline;
        // Spacing multiplier: widens the center-to-center pitch (and crosshair step)
        // without touching body width. Re-clamp because it changes how many bars fit.
        self.coords.set_pitch_scale(next.series.spacing);
        let vp = self.viewport();
        let clamped = self.clamp_viewport(vp.bar_spacing, vp.right_offset);
        self.coords.set_viewport(clamped);
        self.target_bar_spacing = clamped.bar_spacing;

        // Re-derive the candle-colored theme and repaint the chrome that mirrors it.
        self.theme = derive_theme(&self.theme, &self.candle_up, &self.candle_down);
        self.push_theme_to_host();
        self.invalidate(InvalidateLevel::Full);
    }

    /// Theme side effects shared by `set_theme` and `apply_config`: the host's chrome
    /// follows (toolbar, dialogs, legend, buttons, the container background), and the
    /// attribution ink may flip.
    pub(crate) fn push_theme_to_host(&mut self) {
        if !self.mounted {
            return;
        }
        let chrome = self.chrome_theme();
        self.host.apply_chrome_theme(&chrome);
        self.host
            .set_background(&theme_string(&self.theme, "background"));
        if let Some(h) = &self.user_drawings {
            // Re-theme the docked drawing toolbar on the STABLE chrome surface, so
            // editing the plot background never bleeds into the toolbar.
            h.set_theme(&chrome);
        }
        self.attribution_sent = None;
        self.position_attribution();
    }

    // ── theme ──

    /// A theme's up and down colors are the series' defaults: on a theme switch, every
    /// series color still on the previous theme's moves to the next theme's, and a
    /// color a host or user chose stays. Colors left to inherit (`None`) keep inheriting.
    pub(crate) fn follow_theme_candles(&mut self, prev: &Value, next: &Value) {
        let (pu, pd) = (
            theme_string(prev, "upColor"),
            theme_string(prev, "downColor"),
        );
        let (nu, nd) = (
            theme_string(next, "upColor"),
            theme_string(next, "downColor"),
        );
        if pu == nu && pd == nd {
            return;
        }
        let prev_c = CandleColors {
            up_color: pu,
            down_color: pd,
        };
        let next_c = CandleColors {
            up_color: nu,
            down_color: nd,
        };
        let follow = theme_color_follower(&prev_c, &next_c);
        let opt = |c: &Option<String>| c.as_deref().map(&follow);
        self.candle_up = follow(&self.candle_up);
        self.candle_down = follow(&self.candle_down);
        let s = &mut self.scene.style;
        s.candle = CandleStyle {
            border_up_color: opt(&s.candle.border_up_color),
            border_down_color: opt(&s.candle.border_down_color),
            wick_up_color: opt(&s.candle.wick_up_color),
            wick_down_color: opt(&s.candle.wick_down_color),
            ..s.candle.clone()
        };
        s.bars = BarsStyle {
            up_color: opt(&s.bars.up_color),
            down_color: opt(&s.bars.down_color),
        };
        s.line = LineSeriesStyle {
            color: opt(&s.line.color),
            ..s.line.clone()
        };
        s.area = AreaSeriesStyle {
            line_color: opt(&s.area.line_color),
            top_color: opt(&s.area.top_color),
            ..s.area.clone()
        };
        let follow_columns = columns_color_follower(&prev_c, &next_c);
        s.columns = ColumnsSeriesStyle {
            up_color: s.columns.up_color.as_deref().map(&follow_columns),
            down_color: s.columns.down_color.as_deref().map(&follow_columns),
        };
    }

    pub(crate) fn set_theme(&mut self, theme: &Value) {
        let app = self.app_theme.clone();
        self.follow_theme_candles(&app, theme);
        self.theme = derive_theme(theme, &self.candle_up, &self.candle_down);
        self.app_theme = theme.clone();
        // A full app-theme swap re-bases the chrome surface.
        self.surface_background = theme_string(theme, "background");
        self.surface_text_color = theme_string(theme, "textColor");
        self.push_theme_to_host();
        self.invalidate(InvalidateLevel::Full);
    }
}
