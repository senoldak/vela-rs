//! A renderer-agnostic count of the graphic elements the core generated for the
//! mounted indicators: the deterministic oracle signal. It confirms the core
//! *produced* the elements (engine + model mapping), independent of any
//! renderer. Whether a renderer actually *drew* them is a separate (visual)
//! check.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::IndicatorModel;

/// The graphic elements one indicator generated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorSummary {
    pub id: String,
    pub title: String,
    pub overlay: bool,
    pub pane_id: Option<String>,
    /// A native (core-computed) indicator; `false` for a script indicator.
    pub native: bool,
    pub native_type: Option<String>,
    /// Value-series counts keyed by kind
    /// (line/area/step/histogram/columns/circles/cross/candle/bar/markers).
    pub series: BTreeMap<String, usize>,
    pub fills: usize,
    pub backgrounds: usize,
    pub price_lines: usize,
    pub lines: usize,
    pub boxes: usize,
    pub labels: usize,
    pub polylines: usize,
    pub linefills: usize,
    pub tables: usize,
    pub bar_colors: usize,
    pub trades: usize,
    pub inputs: usize,
    /// Elements flagged `force_overlay` (routed to the price pane), across every kind.
    pub forced_overlay: usize,
}

/// Sums across all indicators, for convenient whole-scene assertions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneTotals {
    pub panes: usize,
    pub series: usize,
    pub fills: usize,
    pub backgrounds: usize,
    pub price_lines: usize,
    pub lines: usize,
    pub boxes: usize,
    pub labels: usize,
    pub polylines: usize,
    pub linefills: usize,
    pub tables: usize,
    pub bar_colors: usize,
    pub trades: usize,
    pub forced_overlay: usize,
}

/// A snapshot of everything the Vela core has generated for the mounted indicators.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneInspection {
    pub indicators: Vec<IndicatorSummary>,
    pub totals: SceneTotals,
}

/// Count the renderer-neutral elements of one indicator model.
pub fn summarize_model(model: &IndicatorModel) -> IndicatorSummary {
    let mut series: BTreeMap<String, usize> = BTreeMap::new();
    for s in &model.series {
        *series.entry(s.kind().as_str().to_string()).or_default() += 1;
    }
    let count = |flags: &mut dyn Iterator<Item = bool>| flags.filter(|f| *f).count();
    let forced_overlay = count(&mut model.series.iter().map(|x| x.overlay))
        + count(&mut model.fills.iter().map(|x| x.overlay))
        + count(&mut model.backgrounds.iter().map(|x| x.overlay))
        + count(&mut model.lines.iter().map(|x| x.overlay))
        + count(&mut model.boxes.iter().map(|x| x.overlay))
        + count(&mut model.labels.iter().map(|x| x.overlay))
        + count(&mut model.polylines.iter().map(|x| x.overlay))
        + count(&mut model.linefills.iter().map(|x| x.overlay))
        + count(&mut model.tables.iter().map(|x| x.overlay));
    IndicatorSummary {
        id: model.id.clone(),
        title: model.title.clone(),
        overlay: model.overlay,
        pane_id: model.pane_id.clone(),
        native: model.native.is_some(),
        native_type: model.native.as_ref().map(|n| n.indicator_type.clone()),
        series,
        fills: model.fills.len(),
        backgrounds: model.backgrounds.len(),
        price_lines: model.price_lines.len(),
        lines: model.lines.len(),
        boxes: model.boxes.len(),
        labels: model.labels.len(),
        polylines: model.polylines.len(),
        linefills: model.linefills.len(),
        tables: model.tables.len(),
        bar_colors: model.bar_colors.len(),
        trades: model.trades.len(),
        inputs: model.inputs.len(),
        forced_overlay,
    }
}

/// Aggregate indicator models into a full scene inspection.
pub fn inspect_models(models: &[IndicatorModel]) -> SceneInspection {
    let indicators: Vec<IndicatorSummary> = models.iter().map(summarize_model).collect();
    let sum = |pick: fn(&IndicatorSummary) -> usize| indicators.iter().map(pick).sum::<usize>();
    let panes = indicators
        .iter()
        .map(|s| s.pane_id.as_deref().unwrap_or("price"))
        .collect::<BTreeSet<_>>()
        .len();
    let totals = SceneTotals {
        panes,
        series: sum(|s| s.series.values().sum()),
        fills: sum(|s| s.fills),
        backgrounds: sum(|s| s.backgrounds),
        price_lines: sum(|s| s.price_lines),
        lines: sum(|s| s.lines),
        boxes: sum(|s| s.boxes),
        labels: sum(|s| s.labels),
        polylines: sum(|s| s.polylines),
        linefills: sum(|s| s.linefills),
        tables: sum(|s| s.tables),
        bar_colors: sum(|s| s.bar_colors),
        trades: sum(|s| s.trades),
        forced_overlay: sum(|s| s.forced_overlay),
    };
    SceneInspection { indicators, totals }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Background, BarColor, LineLikeKind, LineLikeStyle, LineStyle, NativeTag, PaneHint,
        PriceLine, SeriesData, SeriesPoint, SeriesSpec,
    };

    fn series(id: &str, kind: LineLikeKind, overlay: bool) -> SeriesSpec {
        SeriesSpec {
            id: id.into(),
            title: id.into(),
            display_title: None,
            pane_id: "p".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay,
            data: SeriesData::LineLike {
                kind,
                points: Vec::<SeriesPoint>::new(),
                style: LineLikeStyle {
                    color: "#000".into(),
                    width: 1.0,
                    line_style: LineStyle::Solid,
                    base: None,
                },
            },
        }
    }

    fn markers(id: &str) -> SeriesSpec {
        SeriesSpec {
            data: SeriesData::Markers {
                markers: Vec::new(),
            },
            ..series(id, LineLikeKind::Line, false)
        }
    }

    fn model(id: &str) -> IndicatorModel {
        IndicatorModel {
            id: id.into(),
            title: id.to_uppercase(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: false,
            pane_hint: PaneHint::New,
            native: None,
            legend: None,
            pane_axis: None,
            pane_id: None,
            own_scale: false,
            anchor_time: None,
            series: Vec::new(),
            fills: Vec::new(),
            backgrounds: Vec::new(),
            price_lines: Vec::new(),
            lines: Vec::new(),
            boxes: Vec::new(),
            labels: Vec::new(),
            polylines: Vec::new(),
            linefills: Vec::new(),
            tables: Vec::new(),
            bar_colors: Vec::new(),
            trades: Vec::new(),
            inputs: Vec::new(),
            input_values: Default::default(),
            props: Vec::new(),
            prop_values: Default::default(),
        }
    }

    fn background(overlay: bool) -> Background {
        Background {
            id: "b".into(),
            pane_id: "p".into(),
            from: 0,
            to: 1,
            color: "#111".into(),
            overlay,
        }
    }

    fn price_line() -> PriceLine {
        PriceLine {
            id: "l".into(),
            pane_id: "p".into(),
            price: 1.0,
            color: None,
            line_style: None,
            width: None,
            title: None,
            display_title: None,
        }
    }

    #[test]
    fn an_empty_model_summarizes_to_zeros() {
        let summary = summarize_model(&model("a"));
        assert_eq!(summary.id, "a");
        assert_eq!(summary.title, "A");
        assert!(!summary.native && summary.native_type.is_none() && summary.pane_id.is_none());
        assert!(summary.series.is_empty());
        assert_eq!(summary.forced_overlay, 0);
    }

    #[test]
    fn series_are_counted_per_kind_and_overlays_across_every_element_kind() {
        let mut m = model("a");
        m.series = vec![
            series("s1", LineLikeKind::Line, true),
            series("s2", LineLikeKind::Line, false),
            series("s3", LineLikeKind::Histogram, true),
            markers("m"),
        ];
        m.backgrounds = vec![background(true), background(false)];
        m.price_lines = vec![price_line()];
        m.bar_colors = vec![BarColor {
            time: 1,
            color: "#fff".into(),
        }];
        m.pane_id = Some("pane-2".into());
        m.overlay = true;
        let summary = summarize_model(&m);
        assert_eq!(
            summary.series,
            BTreeMap::from([
                ("histogram".to_string(), 1),
                ("line".to_string(), 2),
                ("markers".to_string(), 1),
            ])
        );
        assert_eq!(summary.backgrounds, 2);
        assert_eq!(summary.price_lines, 1);
        assert_eq!(summary.bar_colors, 1);
        assert_eq!(summary.pane_id.as_deref(), Some("pane-2"));
        assert!(summary.overlay);
        // Two flagged series + one flagged background; price lines carry no flag.
        assert_eq!(summary.forced_overlay, 3);
    }

    #[test]
    fn a_native_model_reports_its_type() {
        let mut m = model("v");
        m.native = Some(NativeTag {
            indicator_type: "volume".into(),
            channel: None,
        });
        let summary = summarize_model(&m);
        assert!(summary.native);
        assert_eq!(summary.native_type.as_deref(), Some("volume"));
    }

    #[test]
    fn a_scene_sums_the_indicators_and_counts_distinct_panes() {
        let mut a = model("a");
        a.series = vec![series("s", LineLikeKind::Line, true)];
        a.backgrounds = vec![background(false)];
        // No pane id reads as the price pane.
        let mut b = model("b");
        b.pane_id = Some("price".into());
        b.series = vec![series("t", LineLikeKind::Area, false), markers("m")];
        let mut c = model("c");
        c.pane_id = Some("pane-2".into());
        c.price_lines = vec![price_line(), price_line()];
        let scene = inspect_models(&[a, b, c]);
        assert_eq!(scene.indicators.len(), 3);
        assert_eq!(
            scene.totals,
            SceneTotals {
                panes: 2,
                series: 3,
                backgrounds: 1,
                price_lines: 2,
                forced_overlay: 1,
                ..SceneTotals::default()
            }
        );
    }

    #[test]
    fn an_empty_scene_has_no_panes() {
        assert_eq!(inspect_models(&[]), SceneInspection::default());
    }
}
