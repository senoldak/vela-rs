//! The data-window readout and the legend values: the hovered (or latest) bar's
//! date, OHLCV and every indicator's value there, as data. The legend and the
//! data-window panel DOM are the host's.

use std::borrow::Cow;
use std::sync::OnceLock;

use crate::format::{CompactFormatOptions, FormatSurface, ResolvedFormatters, english_format};
use crate::i18n::{MessageArg, MessageLike, msg, text_of};
use crate::model::{IndicatorModel, Ohlcv, SeriesData, SeriesSpec, SeriesSurface};
use crate::ports::chart_renderer::{
    DataWindowGroup, DataWindowOhlc, DataWindowReadout, DataWindowRow,
};
use crate::render::backdrop::scene_intl;
use crate::render::chrome::ticks::{AxisScale, format_price_label};
use crate::render::chrome::tz::{display_zone, zoned_ms};
use crate::render::core::scene_graph::PaneNode;

use super::host::LegendPlotValue;
use super::state::State;
use super::util::format_stamp_parts;

fn english() -> &'static ResolvedFormatters {
    static ENGLISH: OnceLock<ResolvedFormatters> = OnceLock::new();
    ENGLISH.get_or_init(english_format)
}

fn text(s: &str) -> MessageLike {
    MessageLike::Text(Cow::Owned(s.to_string()))
}

fn format_surface(surface: SeriesSurface) -> FormatSurface {
    if surface == SeriesSurface::Legend {
        FormatSurface::Legend
    } else {
        FormatSurface::DataWindow
    }
}

/// `array[index]` for a signed index.
fn slot<T>(items: &[T], index: i64) -> Option<&T> {
    usize::try_from(index).ok().and_then(|i| items.get(i))
}

impl State {
    /// An indicator's full name as the chart shows it.
    pub(crate) fn model_title(&self, model: &IndicatorModel) -> String {
        let message = model
            .display_title
            .clone()
            .unwrap_or_else(|| text(&model.title));
        self.text_of(&message)
    }

    /// The compact name the legend chip, the settings dialog and the object tree show.
    pub(crate) fn legend_title(&self, model: &IndicatorModel) -> String {
        let short = model
            .display_shorttitle
            .clone()
            .or_else(|| model.shorttitle.as_deref().map(text));
        match short {
            Some(m) => self.text_of(&m),
            None => self.model_title(model),
        }
    }

    /// A series' name as the legend and the data window show it.
    pub(crate) fn series_title(&self, s: &SeriesSpec, model: &IndicatorModel) -> String {
        if let Some(d) = &s.display_title {
            return self.text_of(d);
        }
        if s.title.is_empty() {
            self.model_title(model)
        } else {
            s.title.clone()
        }
    }

    pub(crate) fn data_window_price_pane(&self) -> Option<&PaneNode> {
        self.scene
            .ordered_panes()
            .into_iter()
            .find(|p| p.kind == crate::model::PaneKind::Price)
    }

    fn data_window_fmt(&self, v: f64, pane: Option<&PaneNode>, surface: FormatSurface) -> String {
        match pane {
            Some(p) => format_price_label(
                &AxisScale::from(&p.scale),
                p.bounds.height,
                v,
                self.scene.price_mintick,
                self.scene.format.as_ref().unwrap_or_else(|| english()),
                surface,
            ),
            None => crate::util::js_number(v),
        }
    }

    /// A volume in the data window and the volume legend row (`1.20M`: two decimals kept).
    fn format_volume(&self, v: f64, surface: FormatSurface) -> String {
        self.scene
            .format
            .as_ref()
            .unwrap_or_else(|| english())
            .compact(
                v,
                &CompactFormatOptions {
                    surface: Some(surface),
                    decimals: Some(2.0),
                    trim_zeros: Some(false),
                },
            )
    }

    fn data_window_ohlc(
        &self,
        bar: Option<&Ohlcv>,
        pane: Option<&PaneNode>,
    ) -> Option<DataWindowOhlc> {
        let bar = bar?;
        let s = FormatSurface::DataWindow;
        Some(DataWindowOhlc {
            o: self.data_window_fmt(bar.open, pane, s),
            h: self.data_window_fmt(bar.high, pane, s),
            l: self.data_window_fmt(bar.low, pane, s),
            c: self.data_window_fmt(bar.close, pane, s),
            vol: bar.volume.map(|v| self.format_volume(v, s)),
            up: bar.close >= bar.open,
        })
    }

    /// One group per indicator (name = indicator title), each with a row per plot
    /// shown in the data window.
    fn data_window_groups(&self, idx: i64, price_pane: Option<&PaneNode>) -> Vec<DataWindowGroup> {
        let mut groups = Vec::new();
        for model in self.scene.indicators.values() {
            let rows = self.readout_rows_for(model, idx, price_pane, SeriesSurface::DataWindow);
            if !rows.is_empty() {
                groups.push(DataWindowGroup {
                    name: self.model_title(model),
                    rows,
                });
            }
        }
        groups
    }

    /// One indicator's readout at bar `idx` for one value surface (the legend or the
    /// data window): a row per plot shown on that surface, formatted on its pane's
    /// scale. A plot's `display` decides per surface.
    pub(crate) fn readout_rows_for(
        &self,
        model: &IndicatorModel,
        idx: i64,
        price_pane: Option<&PaneNode>,
        surface: SeriesSurface,
    ) -> Vec<DataWindowRow> {
        // The volume native draws through its bespoke layer and mounts a series-less
        // model, so its readout comes straight from the bar's volume.
        if model
            .native
            .as_ref()
            .is_some_and(|n| n.indicator_type == "volume")
        {
            return self.volume_readout_rows(model, idx, format_surface(surface));
        }
        let pane = model
            .pane_id
            .as_deref()
            .and_then(|id| self.scene.panes.get(id))
            .or(price_pane);
        let off = self.scene.offset_of(&model.id);
        let at = idx - off;
        let theme_up = super::util::theme_string(&self.theme, "upColor");
        let mut rows = Vec::new();
        for s in &model.series {
            if !s.shown_on(surface) {
                continue;
            }
            let (value, color) = match &s.data {
                SeriesData::Candle { bars, style, .. } => (
                    slot(bars, at).map(|b| b.close),
                    style
                        .as_ref()
                        .and_then(|st| st.up.clone())
                        .unwrap_or_else(|| theme_up.clone()),
                ),
                SeriesData::LineLike { points, style, .. } => {
                    let p = slot(points, at);
                    (
                        p.and_then(|p| p.value),
                        p.and_then(|p| p.color.clone())
                            .unwrap_or_else(|| style.color.clone()),
                    )
                }
                SeriesData::Markers { .. } => continue, // markers carry no scalar readout
            };
            let Some(value) = value.filter(|v| v.is_finite()) else {
                continue;
            };
            rows.push(DataWindowRow {
                label: self.series_title(s, model),
                value: self.data_window_fmt(value, pane, format_surface(surface)),
                color,
            });
        }
        rows
    }

    /// The volume indicator's readout: the bar's volume, tinted with the layer's own
    /// direction colors. Hiding volume keeps its model in the scene (only the layer is
    /// suppressed), so the hidden flag is checked here rather than by model absence.
    fn volume_readout_rows(
        &self,
        model: &IndicatorModel,
        idx: i64,
        surface: FormatSurface,
    ) -> Vec<DataWindowRow> {
        if self.volume_hidden {
            return Vec::new();
        }
        let Some(bar) = slot(&self.bars, idx) else {
            return Vec::new();
        };
        let Some(vol) = bar.volume.filter(|v| v.is_finite()) else {
            return Vec::new();
        };
        let cfg = self.scene.volume_layer.as_ref();
        let color = if bar.close >= bar.open {
            cfg.map(|c| c.up_color.clone())
                .unwrap_or_else(|| super::util::theme_string(&self.theme, "upColor"))
        } else {
            cfg.map(|c| c.down_color.clone())
                .unwrap_or_else(|| super::util::theme_string(&self.theme, "downColor"))
        };
        vec![DataWindowRow {
            label: self.model_title(model),
            value: self.format_volume(vol, surface),
            color,
        }]
    }

    /// The data-window readout: the hovered bar's date/time and OHLCV plus every
    /// indicator's value there, or the latest bar when the cursor is off the plot.
    pub(crate) fn data_window_readout(&self) -> DataWindowReadout {
        let n = self.bars.len() as i64;
        if n == 0 {
            return DataWindowReadout {
                date: String::new(),
                time: String::new(),
                ohlc: None,
                groups: Vec::new(),
            };
        }
        let idx = self.hover_logical.unwrap_or(n - 1);
        let bar = slot(&self.bars, idx);
        let price_pane = self.data_window_price_pane();
        let (date, time) = match bar {
            Some(b) => format_stamp_parts(self.zoned_ms_of(b.time as f64)),
            None => (String::new(), String::new()),
        };
        DataWindowReadout {
            date,
            time,
            ohlc: self.data_window_ohlc(bar, price_pane),
            groups: self.data_window_groups(idx, price_pane),
        }
    }

    /// `ms` shifted so its UTC fields read the wall clock in the chart's display zone.
    pub(crate) fn zoned_ms_of(&self, ms: f64) -> f64 {
        let zone = display_zone(&self.scene.timezone, self.coords.bar_interval());
        zoned_ms(&*scene_intl(&self.scene), ms, zone)
    }

    /// Refresh the plot values beside every legend title: the same bar the data
    /// window reads, filtered to the plots whose `display` includes the legend. A
    /// hidden indicator has no scene model, so its row is absent and its readout clears.
    pub(crate) fn update_legend_values(&mut self) {
        if !self.mounted {
            return;
        }
        let n = self.bars.len() as i64;
        let mut values: Vec<(String, Vec<LegendPlotValue>)> = Vec::new();
        if n > 0 {
            let idx = self.hover_logical.unwrap_or(n - 1);
            let price_pane = self.data_window_price_pane();
            for model in self.scene.indicators.values() {
                let rows = self.readout_rows_for(model, idx, price_pane, SeriesSurface::Legend);
                values.push((
                    model.id.clone(),
                    rows.into_iter()
                        .map(|r| LegendPlotValue {
                            value: r.value,
                            color: r.color,
                        })
                        .collect(),
                ));
            }
        }
        self.host.legend_plot_values(&values);
    }

    /// List the legend rows the way the indicators stack, front-most first.
    pub(crate) fn sync_legend_order(&mut self) {
        let ids: Vec<String> = self
            .scene
            .indicator_z_order()
            .into_iter()
            .map(|e| e.id)
            .collect();
        if self.legend_order_sent.as_ref() != Some(&ids) {
            self.legend_order_sent = Some(ids.clone());
            self.host.legend_order(&ids);
        }
    }

    /// Update the ARIA live region with a spoken summary of bar `idx` (date + OHLC +
    /// values).
    pub(crate) fn announce_bar(&mut self, idx: i64) {
        if !self.mounted || !self.keyboard_enabled {
            return;
        }
        let bar = slot(&self.bars, idx);
        let price_pane = self.data_window_price_pane();
        let ohlc = self.data_window_ohlc(bar, price_pane);
        let mut parts: Vec<String> = Vec::new();
        if let Some(b) = bar {
            let (d, t) = format_stamp_parts(self.zoned_ms_of(b.time as f64));
            parts.push(format!("{d} {t}"));
        }
        if let Some(o) = ohlc {
            let m = msg(
                "chart.a11y.barSummary",
                "Open {open}, High {high}, Low {low}, Close {close}",
            );
            parts.push(text_of(
                self.i18n.as_deref(),
                &m,
                &[
                    ("open", MessageArg::from(o.o)),
                    ("high", MessageArg::from(o.h)),
                    ("low", MessageArg::from(o.l)),
                    ("close", MessageArg::from(o.c)),
                ],
            ));
        }
        for group in self.data_window_groups(idx, price_pane) {
            for row in group.rows {
                let m = msg("chart.a11y.valueSummary", "{name} {value}");
                parts.push(text_of(
                    self.i18n.as_deref(),
                    &m,
                    &[
                        ("name", MessageArg::from(row.label)),
                        ("value", MessageArg::from(row.value)),
                    ],
                ));
            }
        }
        self.host.announce(&parts.join(". "));
    }
}
