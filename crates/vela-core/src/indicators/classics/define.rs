//! The classic-indicator adapter: each catalog entry is metadata plus a PURE
//! `compute(bars, inputs)` returning bar-aligned value arrays, and this module
//! wraps it into a [`NativeIndicatorDescriptor`]: lifecycle, stable ids,
//! series-point conversion and fills all handled once. Keeping the computes
//! pure is what makes the whole catalog testable without a chart.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::i18n::{MessageLike, message_only, source_text};
use crate::indicators::{
    IndicatorStatus, NativeIndicator, NativeIndicatorContext, NativeIndicatorDescriptor,
    NativeIndicatorOutput, VisibleRange,
};
use crate::model::{
    DrawingPolyline, Fill, FillGradientStop, InputCondition, InputDisplay, InputGroupHelp,
    InputMultiple, InputSchema, InputType, InputValue, Inputs, LineLikeKind, LineLikeStyle,
    LineStyle, MarkerPoint, Ohlcv, PaneHint, PolylinePoint, PriceLine, SeriesData, SeriesDisplay,
    SeriesPoint, SeriesSpec, stable_series_id,
};

use super::labels::classic_input_labels;

/// One plotted value series; `values` align to bars by index, NaN = whitespace.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassicPlot {
    /// Stable identity of the plot within the indicator (bands reference it).
    pub key: String,
    /// The plot's name. Its English text is the series' identity (its id).
    pub title: MessageLike,
    /// Shown instead of `title` where the chart names the plot, for a plot
    /// whose shown name differs from its English identity.
    pub display_title: Option<MessageLike>,
    /// Series kind; default `Line`.
    pub kind: LineLikeKind,
    pub values: Vec<f64>,
    pub color: String,
    /// Line width; `None` ⇒ 1.
    pub width: Option<f64>,
    /// `None` ⇒ solid.
    pub line_style: Option<LineStyle>,
    /// Baseline for histogram/area kinds.
    pub base: Option<f64>,
    /// Per-bar color override, aligned to bars by index.
    pub colors: Option<Vec<Option<String>>>,
    /// Render on the price pane even when the indicator owns a study pane.
    pub overlay: bool,
    /// Per-surface visibility. A plot shown nowhere still anchors a
    /// [`ClassicBand`]: that is how a zone shaded against a constant is
    /// expressed without a line in the pane or a row in the legend.
    pub display: Option<SeriesDisplay>,
}

impl ClassicPlot {
    /// A solid 1px line plot; refine it with the builder methods.
    pub fn new(
        key: impl Into<String>,
        title: impl Into<MessageLike>,
        values: Vec<f64>,
        color: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            display_title: None,
            kind: LineLikeKind::Line,
            values,
            color: color.into(),
            width: None,
            line_style: None,
            base: None,
            colors: None,
            overlay: false,
            display: None,
        }
    }

    pub fn display_title(mut self, title: impl Into<MessageLike>) -> Self {
        self.display_title = Some(title.into());
        self
    }

    pub fn kind(mut self, kind: LineLikeKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn width(mut self, width: f64) -> Self {
        self.width = Some(width);
        self
    }

    pub fn line_style(mut self, style: LineStyle) -> Self {
        self.line_style = Some(style);
        self
    }

    pub fn base(mut self, base: f64) -> Self {
        self.base = Some(base);
        self
    }

    pub fn colors(mut self, colors: Vec<Option<String>>) -> Self {
        self.colors = Some(colors);
        self
    }

    pub fn overlay(mut self, overlay: bool) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn display(mut self, display: SeriesDisplay) -> Self {
        self.display = Some(display);
        self
    }
}

/// A soft band filled between two plots (referenced by plot key).
#[derive(Clone, Debug, PartialEq)]
pub struct ClassicBand {
    pub key: String,
    pub from: String,
    pub to: String,
    pub color: String,
    /// Per-bar solid color, aligned to bars by index; overrides `color` where set.
    pub colors: Option<Vec<Option<String>>>,
    /// Per-bar vertical gradient (a two-tone wash across the band), aligned by index.
    pub gradient: Option<Vec<Option<FillGradientStop>>>,
}

impl ClassicBand {
    pub fn new(
        key: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        color: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            from: from.into(),
            to: to.into(),
            color: color.into(),
            colors: None,
            gradient: None,
        }
    }

    pub fn colors(mut self, colors: Vec<Option<String>>) -> Self {
        self.colors = Some(colors);
        self
    }

    pub fn gradient(mut self, gradient: Vec<Option<FillGradientStop>>) -> Self {
        self.gradient = Some(gradient);
        self
    }
}

/// A fixed horizontal level on the indicator's pane.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassicLevel {
    pub key: String,
    pub price: f64,
    pub color: Option<String>,
    /// `None` ⇒ dashed.
    pub line_style: Option<LineStyle>,
    pub title: Option<MessageLike>,
}

impl ClassicLevel {
    pub fn new(key: impl Into<String>, price: f64) -> Self {
        Self {
            key: key.into(),
            price,
            color: None,
            line_style: None,
            title: None,
        }
    }

    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    pub fn line_style(mut self, style: LineStyle) -> Self {
        self.line_style = Some(style);
        self
    }

    pub fn title(mut self, title: impl Into<MessageLike>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// A free-form path (e.g. a zigzag's pivot-to-pivot legs): a
/// [`DrawingPolyline`] without its id and pane, which the adapter stamps.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassicPolyline {
    pub key: String,
    pub points: Vec<PolylinePoint>,
    pub curved: bool,
    pub closed: bool,
    pub line_color: Option<String>,
    pub fill_color: Option<String>,
    pub line_width: f64,
    pub line_style: LineStyle,
    pub arrow_left: bool,
    pub arrow_right: bool,
    pub overlay: bool,
}

/// What one compute pass yields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassicOutput {
    pub plots: Vec<ClassicPlot>,
    pub bands: Vec<ClassicBand>,
    pub levels: Vec<ClassicLevel>,
    pub markers: Vec<MarkerPoint>,
    pub polylines: Vec<ClassicPolyline>,
}

/// One input of a catalog entry: an input schema whose title and tooltip may
/// be translatable messages.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassicInput {
    pub key: String,
    pub title: MessageLike,
    pub input_type: InputType,
    pub defval: InputValue,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub options: Option<Vec<String>>,
    pub group: Option<String>,
    pub inline: Option<String>,
    pub tab: Option<String>,
    pub when: Vec<InputCondition>,
    pub tooltip: Option<MessageLike>,
    pub display: Option<InputDisplay>,
    pub multiple: Option<InputMultiple>,
    pub group_help: Option<InputGroupHelp>,
}

impl ClassicInput {
    /// An input with only the required fields set.
    pub fn new(
        key: impl Into<String>,
        title: impl Into<MessageLike>,
        input_type: InputType,
        defval: impl Into<InputValue>,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            input_type,
            defval: defval.into(),
            min: None,
            max: None,
            step: None,
            options: None,
            group: None,
            inline: None,
            tab: None,
            when: Vec::new(),
            tooltip: None,
            display: None,
            multiple: None,
            group_help: None,
        }
    }

    /// Show this input only while every condition passes.
    pub fn when(mut self, when: Vec<InputCondition>) -> Self {
        self.when = when;
        self
    }

    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn inline(mut self, inline: impl Into<String>) -> Self {
        self.inline = Some(inline.into());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<MessageLike>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

/// Display labels shared by many inputs, keyed by the identity they label.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputLabels {
    /// Section heading per `group` name.
    pub groups: BTreeMap<String, MessageLike>,
    /// Shown label per option value.
    pub options: BTreeMap<String, MessageLike>,
}

/// Resolve a localizable input into the schema the chart reads: the English
/// source stays the identity (`title`, `tooltip`, `group`, option values) and
/// the translatable side lands in `display`.
pub fn localize_input(input: &ClassicInput, labels: &InputLabels) -> InputSchema {
    let mut display = input.display.clone().unwrap_or_default();
    if display.title.is_none() {
        display.title = message_only(Some(&input.title)).cloned();
    }
    if display.tooltip.is_none() {
        display.tooltip = message_only(input.tooltip.as_ref()).cloned();
    }
    if display.group.is_none() {
        display.group = input
            .group
            .as_ref()
            .and_then(|g| labels.groups.get(g))
            .cloned();
    }
    if display.options.is_none()
        && let Some(options) = &input.options
    {
        let shown: BTreeMap<String, MessageLike> = options
            .iter()
            .filter_map(|v| {
                labels
                    .options
                    .get(v)
                    .map(|label| (v.clone(), label.clone()))
            })
            .collect();
        if !shown.is_empty() {
            display.options = Some(shown);
        }
    }
    InputSchema {
        key: input.key.clone(),
        title: source_text(&input.title),
        input_type: input.input_type,
        defval: input.defval.clone(),
        min: input.min,
        max: input.max,
        step: input.step,
        options: input.options.clone(),
        group: input.group.clone(),
        inline: input.inline.clone(),
        tab: input.tab.clone(),
        when: input.when.clone(),
        tooltip: input.tooltip.as_ref().map(source_text),
        display: if display.is_empty() {
            None
        } else {
            Some(display)
        },
        multiple: input.multiple.clone(),
        group_help: input.group_help.clone(),
    }
}

/// The pure compute of a catalog entry.
pub type ClassicCompute = Arc<dyn Fn(&[Ohlcv], &Inputs) -> ClassicOutput + Send + Sync>;

/// A catalog entry: metadata + input schema + the pure compute.
#[derive(Clone)]
pub struct ClassicIndicatorSpec {
    pub indicator_type: &'static str,
    pub title: MessageLike,
    pub short_title: Option<MessageLike>,
    /// true = draws over the price candles; false = its own pane below.
    pub overlay: bool,
    pub inputs: Vec<ClassicInput>,
    pub compute: ClassicCompute,
}

impl ClassicIndicatorSpec {
    pub fn new(
        indicator_type: &'static str,
        title: impl Into<MessageLike>,
        overlay: bool,
        inputs: Vec<ClassicInput>,
        compute: impl Fn(&[Ohlcv], &Inputs) -> ClassicOutput + Send + Sync + 'static,
    ) -> Self {
        Self {
            indicator_type,
            title: title.into(),
            short_title: None,
            overlay,
            inputs,
            compute: Arc::new(compute),
        }
    }

    pub fn short_title(mut self, short_title: impl Into<MessageLike>) -> Self {
        self.short_title = Some(short_title.into());
        self
    }

    /// Run the pure compute.
    pub fn compute(&self, bars: &[Ohlcv], inputs: &Inputs) -> ClassicOutput {
        (self.compute)(bars, inputs)
    }

    /// The declaration default of every input, keyed by input key.
    pub fn default_inputs(&self) -> Inputs {
        self.inputs
            .iter()
            .map(|i| (i.key.clone(), i.defval.clone()))
            .collect()
    }
}

impl std::fmt::Debug for ClassicIndicatorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClassicIndicatorSpec")
            .field("indicator_type", &self.indicator_type)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

/// A finite numeric input, else `fallback`.
pub fn num(inputs: &Inputs, key: &str, fallback: f64) -> f64 {
    match inputs.get(key) {
        Some(InputValue::Number(v)) if v.is_finite() => *v,
        _ => fallback,
    }
}

/// A numeric input as a window length: truncated, negatives clamp to 0.
pub fn len(inputs: &Inputs, key: &str, fallback: usize) -> usize {
    let v = num(inputs, key, fallback as f64);
    if v <= 0.0 { 0 } else { v as usize }
}

/// A non-empty text input, else `fallback`.
pub fn text(inputs: &Inputs, key: &str, fallback: &str) -> String {
    match inputs.get(key) {
        Some(InputValue::Text(v)) if !v.is_empty() => v.clone(),
        _ => fallback.to_string(),
    }
}

/// A boolean input, else `fallback`.
pub fn flag(inputs: &Inputs, key: &str, fallback: bool) -> bool {
    match inputs.get(key) {
        Some(InputValue::Bool(v)) => *v,
        _ => fallback,
    }
}

fn to_points(
    bars: &[Ohlcv],
    values: &[f64],
    colors: Option<&[Option<String>]>,
) -> Vec<SeriesPoint> {
    bars.iter()
        .enumerate()
        .map(|(i, bar)| SeriesPoint {
            time: bar.time,
            value: values.get(i).copied().filter(|v| v.is_finite()),
            color: colors.and_then(|c| c.get(i)).cloned().flatten(),
        })
        .collect()
}

/// Turn one compute pass into the renderer-neutral output: stable ids,
/// whitespace for warm-up NaNs, bands wired to their plots' series ids.
/// Pane ids are left empty; the orchestrator stamps them.
pub fn classic_native_output(
    spec: &ClassicIndicatorSpec,
    bars: &[Ohlcv],
    inputs: &Inputs,
) -> NativeIndicatorOutput {
    let out = spec.compute(bars, inputs);
    let instance_id = spec.indicator_type;
    let mut series: Vec<SeriesSpec> = Vec::with_capacity(out.plots.len() + 1);
    let mut ids_by_key: BTreeMap<&str, String> = BTreeMap::new();
    for (ordinal, plot) in out.plots.iter().enumerate() {
        let title = source_text(&plot.title);
        let id = stable_series_id(instance_id, plot.kind.as_str(), &title, ordinal);
        ids_by_key.insert(plot.key.as_str(), id.clone());
        series.push(SeriesSpec {
            id,
            title,
            display_title: plot
                .display_title
                .clone()
                .or_else(|| message_only(Some(&plot.title)).cloned()),
            pane_id: String::new(),
            z_order: None,
            visible: None,
            display: plot.display,
            overlay: plot.overlay,
            data: SeriesData::LineLike {
                kind: plot.kind,
                points: to_points(bars, &plot.values, plot.colors.as_deref()),
                style: LineLikeStyle {
                    color: plot.color.clone(),
                    width: plot.width.unwrap_or(1.0),
                    line_style: plot.line_style.unwrap_or(LineStyle::Solid),
                    base: plot.base,
                },
            },
        });
    }
    if !out.markers.is_empty() {
        let name = spec.short_title.as_ref().unwrap_or(&spec.title);
        series.push(SeriesSpec {
            id: stable_series_id(instance_id, "markers", "markers", 0),
            title: source_text(name),
            display_title: message_only(Some(name)).cloned(),
            pane_id: String::new(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Markers {
                markers: out.markers.clone(),
            },
        });
    }
    let fills: Vec<Fill> = out
        .bands
        .iter()
        .enumerate()
        .filter_map(|(ordinal, band)| {
            let from = ids_by_key.get(band.from.as_str())?;
            let to = ids_by_key.get(band.to.as_str())?;
            Some(Fill {
                id: stable_series_id(instance_id, "fill", &band.key, ordinal),
                pane_id: String::new(),
                from_series_id: from.clone(),
                to_series_id: to.clone(),
                color: Some(band.color.clone()),
                colors: band.colors.clone(),
                gradient: band.gradient.clone(),
                overlay: false,
            })
        })
        .collect();
    let price_lines: Vec<PriceLine> = out
        .levels
        .iter()
        .enumerate()
        .map(|(ordinal, level)| PriceLine {
            id: stable_series_id(instance_id, "hline", &level.key, ordinal),
            pane_id: String::new(),
            price: level.price,
            color: level.color.clone(),
            line_style: Some(level.line_style.unwrap_or(LineStyle::Dashed)),
            width: Some(1.0),
            title: level.title.as_ref().map(source_text),
            display_title: message_only(level.title.as_ref()).cloned(),
        })
        .collect();
    let polylines: Vec<DrawingPolyline> = out
        .polylines
        .iter()
        .enumerate()
        .map(|(ordinal, poly)| DrawingPolyline {
            id: stable_series_id(instance_id, "polyline", &poly.key, ordinal),
            pane_id: String::new(),
            points: poly.points.clone(),
            curved: poly.curved,
            closed: poly.closed,
            line_color: poly.line_color.clone(),
            fill_color: poly.fill_color.clone(),
            line_width: poly.line_width,
            line_style: poly.line_style,
            arrow_left: poly.arrow_left,
            arrow_right: poly.arrow_right,
            overlay: poly.overlay,
        })
        .collect();
    NativeIndicatorOutput {
        series,
        fills,
        price_lines,
        polylines,
        ..Default::default()
    }
}

struct ClassicIndicator {
    spec: ClassicIndicatorSpec,
    /// False until `start`: a pre-start `set_inputs` must record without computing.
    started: bool,
    inputs: Inputs,
}

impl ClassicIndicator {
    fn recompute(&self, ctx: &mut dyn NativeIndicatorContext) {
        if !self.started {
            return;
        }
        let out = classic_native_output(&self.spec, ctx.bars(), &self.inputs);
        ctx.emit(out);
        ctx.set_status(IndicatorStatus::Idle);
    }
}

impl NativeIndicator for ClassicIndicator {
    fn start(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs) {
        self.started = true;
        self.inputs = inputs;
        self.recompute(ctx);
    }

    fn on_bars(&mut self, ctx: &mut dyn NativeIndicatorContext) {
        self.recompute(ctx);
    }

    fn on_viewport(&mut self, _ctx: &mut dyn NativeIndicatorContext, _range: VisibleRange) {}

    fn set_inputs(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs) {
        self.inputs = inputs;
        self.recompute(ctx);
    }

    /// Nothing to free: the compute reads the bars on demand.
    fn suspend(&mut self) {}

    fn resume(&mut self, ctx: &mut dyn NativeIndicatorContext) {
        self.recompute(ctx);
    }

    fn stop(&mut self, _ctx: &mut dyn NativeIndicatorContext) {}
}

/// Wrap a catalog entry into a registrable native-indicator descriptor.
pub fn classic_descriptor(spec: &ClassicIndicatorSpec) -> NativeIndicatorDescriptor {
    let labels = classic_input_labels();
    let factory_spec = spec.clone();
    NativeIndicatorDescriptor {
        indicator_type: spec.indicator_type.to_string(),
        title: spec.title.clone(),
        short_title: spec.short_title.clone(),
        pane_hint: if spec.overlay {
            PaneHint::Price
        } else {
            PaneHint::New
        },
        overlay: spec.overlay,
        reacts_to_viewport: false,
        // A study's values depend on every bar before them (an EMA's seed, an
        // RSI's smoothing), so it computes once over the whole history.
        waits_for_history: true,
        beta: false,
        legend: None,
        // Studies stack (a 20 and a 200 moving average side by side).
        multi_instance: true,
        inputs_schema: spec
            .inputs
            .iter()
            .map(|i| localize_input(i, labels))
            .collect(),
        factory: Arc::new(move || {
            Box::new(ClassicIndicator {
                spec: factory_spec.clone(),
                started: false,
                inputs: Inputs::new(),
            })
        }),
    }
}
