//! The built-in VOLUME indicator: per-bar volume columns anchored to the BOTTOM
//! of the price pane, colored by bar direction. The columns use their OWN scale
//! (the tallest visible bar spans `heightPct` of the pane), so volume units never
//! touch the price autoscale: which is why this is a bespoke renderer layer, not
//! a model series. All geometry lives in the renderer's volume layer (it reads the
//! chart's bars each frame); this instance only carries lifecycle and settings.

use std::sync::Arc;

use super::classics::{ClassicInput, InputLabels, localize_input};
use super::{
    NativeIndicator, NativeIndicatorContext, NativeIndicatorDescriptor, NativeIndicatorOutput,
    NativeIndicatorRegistry, VisibleRange,
};
use crate::i18n::msg;
use crate::model::volume_layers::VolumeLayerData;
use crate::model::{InputType, InputValue, Inputs, PaneHint};
use crate::palette::{BEARISH, BULLISH};

const DEFAULT_UP: &str = BULLISH;
const DEFAULT_DOWN: &str = BEARISH;
const DEFAULT_HEIGHT_PCT: f64 = 20.0;

pub(super) fn num(v: Option<&InputValue>, fallback: f64) -> f64 {
    match v {
        Some(InputValue::Number(n)) if n.is_finite() => *n,
        _ => fallback,
    }
}

pub(super) fn text(v: Option<&InputValue>, fallback: &str) -> String {
    match v {
        Some(InputValue::Text(s)) if !s.is_empty() => s.clone(),
        _ => fallback.to_string(),
    }
}

/// Resolve the input record into the layer payload (clamped, fraction-normalized).
pub fn volume_layer_data(inputs: &Inputs) -> VolumeLayerData {
    VolumeLayerData {
        up_color: text(inputs.get("upColor"), DEFAULT_UP),
        down_color: text(inputs.get("downColor"), DEFAULT_DOWN),
        height_frac: (num(inputs.get("heightPct"), DEFAULT_HEIGHT_PCT) / 100.0).clamp(0.05, 0.5),
    }
}

/// One volume instance. `started` is false until `start`: a pre-start
/// `set_inputs` / `resume` must record without pushing (a workspace restore
/// applies stored inputs the moment the handle exists).
#[derive(Default)]
struct VolumeIndicator {
    started: bool,
    inputs: Inputs,
}

impl VolumeIndicator {
    fn push(&self, ctx: &mut dyn NativeIndicatorContext) {
        if let Ok(payload) = serde_json::to_value(volume_layer_data(&self.inputs)) {
            ctx.push_data(payload);
        }
    }
}

impl NativeIndicator for VolumeIndicator {
    fn start(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs) {
        self.started = true;
        self.inputs = inputs;
        // Mount the legend row: the layer draws outside the model.
        ctx.emit(NativeIndicatorOutput::default());
        self.push(ctx);
    }

    /// The layer reads the live bar array directly: nothing to recompute here.
    fn on_bars(&mut self, _ctx: &mut dyn NativeIndicatorContext) {}

    /// Visible-range normalization happens per frame in the layer.
    fn on_viewport(&mut self, _ctx: &mut dyn NativeIndicatorContext, _range: VisibleRange) {}

    fn set_inputs(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs) {
        self.inputs = inputs;
        if self.started {
            self.push(ctx);
        }
    }

    /// Hiding is a renderer-layer flag (set via `set_indicator_visible`); no
    /// resources to free.
    fn suspend(&mut self) {}

    fn resume(&mut self, ctx: &mut dyn NativeIndicatorContext) {
        if self.started {
            self.push(ctx);
        }
    }

    fn stop(&mut self, _ctx: &mut dyn NativeIndicatorContext) {}
}

/// Descriptor for the built-in volume indicator. Bar volume needs no capability probe.
pub fn volume_descriptor() -> NativeIndicatorDescriptor {
    let labels = InputLabels::default();
    let mut height = ClassicInput::new(
        "heightPct",
        msg("indicator.volume.heightPct", "Height %"),
        InputType::Int,
        DEFAULT_HEIGHT_PCT,
    )
    .tooltip(msg(
        "indicator.volume.heightPct.tooltip",
        "Panel height the tallest visible bar occupies.",
    ));
    height.min = Some(5.0);
    height.max = Some(50.0);
    height.step = Some(1.0);
    let inputs = [
        ClassicInput::new(
            "upColor",
            msg("indicator.volume.upColor", "Up color"),
            InputType::Color,
            DEFAULT_UP,
        ),
        ClassicInput::new(
            "downColor",
            msg("indicator.volume.downColor", "Down color"),
            InputType::Color,
            DEFAULT_DOWN,
        ),
        height,
    ];
    NativeIndicatorDescriptor {
        indicator_type: "volume".to_string(),
        title: msg("indicator.volume.title", "Volume"),
        short_title: None,
        pane_hint: PaneHint::Price,
        overlay: true,
        reacts_to_viewport: false,
        waits_for_history: false,
        beta: false,
        legend: None,
        multi_instance: false,
        inputs_schema: inputs
            .iter()
            .map(|input| localize_input(input, &labels))
            .collect(),
        factory: Arc::new(|| Box::<VolumeIndicator>::default()),
    }
}

/// Register the built-in volume native indicator (idempotent).
pub fn register_volume(registry: &mut NativeIndicatorRegistry) {
    registry.register(volume_descriptor());
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::model::Ohlcv;

    /// A context recording what the indicator emits and pushes.
    #[derive(Default)]
    pub(super) struct RecordingContext {
        pub bars: Vec<Ohlcv>,
        pub emitted: Vec<NativeIndicatorOutput>,
        pub pushed: Vec<Value>,
    }

    impl NativeIndicatorContext for RecordingContext {
        fn id(&self) -> &str {
            "volume-1"
        }
        fn symbol(&self) -> &str {
            "TEST"
        }
        fn timeframe(&self) -> &str {
            "1"
        }
        fn live(&self) -> bool {
            false
        }
        fn bars(&self) -> &[Ohlcv] {
            &self.bars
        }
        fn emit(&mut self, out: NativeIndicatorOutput) {
            self.emitted.push(out);
        }
        fn push_data(&mut self, data: Value) {
            self.pushed.push(data);
        }
        fn set_status(&mut self, _status: crate::indicators::IndicatorStatus) {}
    }

    fn inputs(pairs: &[(&str, InputValue)]) -> Inputs {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn layer_data_defaults_when_inputs_are_absent() {
        let data = volume_layer_data(&Inputs::new());
        assert_eq!(data.up_color, BULLISH);
        assert_eq!(data.down_color, BEARISH);
        assert_eq!(data.height_frac, 0.2);
    }

    #[test]
    fn layer_data_clamps_the_height_and_ignores_junk() {
        let height = |v: InputValue| volume_layer_data(&inputs(&[("heightPct", v)])).height_frac;
        assert_eq!(height(InputValue::Number(1.0)), 0.05);
        assert_eq!(height(InputValue::Number(99.0)), 0.5);
        assert_eq!(height(InputValue::Number(35.0)), 0.35);
        assert_eq!(height(InputValue::Number(f64::NAN)), 0.2);
        assert_eq!(height(InputValue::Number(f64::INFINITY)), 0.2);
        assert_eq!(height(InputValue::Text("30".into())), 0.2);
        let colors = volume_layer_data(&inputs(&[
            ("upColor", InputValue::Text("#111".into())),
            ("downColor", InputValue::Text(String::new())),
        ]));
        assert_eq!(colors.up_color, "#111");
        assert_eq!(colors.down_color, BEARISH);
    }

    #[test]
    fn the_descriptor_declares_the_original_type() {
        let d = volume_descriptor();
        assert_eq!(d.indicator_type, "volume");
        assert_eq!(d.pane_hint, PaneHint::Price);
        assert!(d.overlay);
        assert!(!d.multi_instance && !d.waits_for_history && !d.beta);
        assert!(d.short_title.is_none() && d.legend.is_none());
        let keys: Vec<&str> = d.inputs_schema.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["upColor", "downColor", "heightPct"]);
        let height = &d.inputs_schema[2];
        assert_eq!(height.title, "Height %");
        assert_eq!(
            (height.min, height.max, height.step),
            (Some(5.0), Some(50.0), Some(1.0))
        );
        assert_eq!(
            height.tooltip.as_deref(),
            Some("Panel height the tallest visible bar occupies.")
        );
        assert_eq!(
            d.default_inputs(),
            inputs(&[
                ("upColor", InputValue::Text(BULLISH.into())),
                ("downColor", InputValue::Text(BEARISH.into())),
                ("heightPct", InputValue::Number(20.0)),
            ])
        );
    }

    #[test]
    fn start_mounts_the_legend_row_then_pushes_the_layer_config() {
        let d = volume_descriptor();
        let mut instance = d.create();
        let mut ctx = RecordingContext::default();
        instance.start(&mut ctx, d.default_inputs());
        assert_eq!(ctx.emitted, [NativeIndicatorOutput::default()]);
        assert_eq!(
            ctx.pushed,
            [json!({ "upColor": BULLISH, "downColor": BEARISH, "heightFrac": 0.2 })]
        );
    }

    #[test]
    fn pre_start_edits_record_without_pushing() {
        let d = volume_descriptor();
        let mut instance = d.create();
        let mut ctx = RecordingContext::default();
        instance.set_inputs(&mut ctx, inputs(&[("heightPct", InputValue::Number(40.0))]));
        instance.resume(&mut ctx);
        assert!(ctx.pushed.is_empty() && ctx.emitted.is_empty());
        // start() replaces the recorded inputs with the orchestrator's replayed values.
        instance.start(&mut ctx, inputs(&[("heightPct", InputValue::Number(10.0))]));
        assert_eq!(ctx.pushed.len(), 1);
        assert_eq!(ctx.pushed[0]["heightFrac"], json!(0.1));
    }

    #[test]
    fn edits_and_resume_after_start_push_the_current_inputs() {
        let d = volume_descriptor();
        let mut instance = d.create();
        let mut ctx = RecordingContext::default();
        instance.start(&mut ctx, d.default_inputs());
        instance.set_inputs(
            &mut ctx,
            inputs(&[("upColor", InputValue::Text("#0f0".into()))]),
        );
        instance.suspend();
        instance.resume(&mut ctx);
        instance.on_bars(&mut ctx);
        instance.on_viewport(&mut ctx, VisibleRange { from: 0, to: 1 });
        instance.stop(&mut ctx);
        assert_eq!(ctx.pushed.len(), 3);
        assert_eq!(ctx.pushed[1]["upColor"], json!("#0f0"));
        assert_eq!(ctx.pushed[2], ctx.pushed[1]);
        assert_eq!(ctx.emitted.len(), 1);
    }

    #[test]
    fn registering_is_idempotent() {
        let mut registry = NativeIndicatorRegistry::new();
        register_volume(&mut registry);
        register_volume(&mut registry);
        assert_eq!(registry.types(), ["volume"]);
    }
}
