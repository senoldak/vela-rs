//! The built-in VPVR (visible-range volume profile) indicator: horizontal
//! volume-by-price rows anchored to the RIGHT edge of the price pane,
//! aggregating every visible bar. Row y-positions are real prices (the shared
//! price scale); row WIDTHS are screen fractions (the profile's own horizontal
//! scale), so the profile never touches the price autoscale: which is why it is a
//! bespoke renderer layer rather than model series. The layer re-buckets per
//! frame from the chart's bars (memoized on the visible range), so the profile
//! tracks pan and zoom without any orchestrator round trip; this instance only
//! carries lifecycle and settings.

use std::sync::Arc;

use super::classics::{ClassicInput, InputLabels, localize_input};
use super::volume::{num, text};
use super::{
    NativeIndicator, NativeIndicatorContext, NativeIndicatorDescriptor, NativeIndicatorOutput,
    NativeIndicatorRegistry, VisibleRange,
};
use crate::i18n::msg;
use crate::model::volume_layers::VpvrLayerData;
use crate::model::{InputType, InputValue, Inputs, PaneHint};
use crate::palette::{ACCENT, BEARISH};

const DEFAULT_ROWS: f64 = 24.0;
const DEFAULT_WIDTH_PCT: f64 = 30.0;
const DEFAULT_UP: &str = ACCENT;
const DEFAULT_DOWN: &str = BEARISH;
const DEFAULT_VALUE_AREA_PCT: f64 = 70.0;

/// Resolve the input record into the layer payload (clamped, fraction-normalized).
pub fn vpvr_layer_data(inputs: &Inputs) -> VpvrLayerData {
    // `Math.round` rounds halves up, `f64::round` away from zero: the same for
    // the positive values the clamp leaves.
    let rows = num(inputs.get("rows"), DEFAULT_ROWS).clamp(2.0, 400.0);
    VpvrLayerData {
        rows: rows.round() as u32,
        width_frac: (num(inputs.get("widthPct"), DEFAULT_WIDTH_PCT) / 100.0).clamp(0.05, 0.9),
        up_color: text(inputs.get("upColor"), DEFAULT_UP),
        down_color: text(inputs.get("downColor"), DEFAULT_DOWN),
        show_poc: inputs.get("showPoc") != Some(&InputValue::Bool(false)),
        value_area_frac: (num(inputs.get("valueAreaPct"), DEFAULT_VALUE_AREA_PCT) / 100.0)
            .clamp(0.0, 1.0),
    }
}

/// One VPVR instance (see the volume indicator for the `started` rule).
#[derive(Default)]
struct VpvrIndicator {
    started: bool,
    inputs: Inputs,
}

impl VpvrIndicator {
    fn push(&self, ctx: &mut dyn NativeIndicatorContext) {
        if let Ok(payload) = serde_json::to_value(vpvr_layer_data(&self.inputs)) {
            ctx.push_data(payload);
        }
    }
}

impl NativeIndicator for VpvrIndicator {
    fn start(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs) {
        self.started = true;
        self.inputs = inputs;
        // Mount the legend row: the layer draws outside the model.
        ctx.emit(NativeIndicatorOutput::default());
        self.push(ctx);
    }

    /// The layer reads the live bar array directly: nothing to recompute here.
    fn on_bars(&mut self, _ctx: &mut dyn NativeIndicatorContext) {}

    /// The layer re-buckets from the visible range per frame: no poke needed.
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

fn int_input(
    key: &'static str,
    title: crate::i18n::MessageLike,
    defval: f64,
    min: f64,
    max: f64,
) -> ClassicInput {
    let mut input = ClassicInput::new(key, title, InputType::Int, defval);
    input.min = Some(min);
    input.max = Some(max);
    input.step = Some(1.0);
    input
}

/// Descriptor for the built-in visible-range volume profile. Bar volume needs no
/// capability probe.
pub fn vpvr_descriptor() -> NativeIndicatorDescriptor {
    let labels = InputLabels::default();
    let inputs = [
        int_input(
            "rows",
            msg("indicator.vpvr.rows", "Rows"),
            DEFAULT_ROWS,
            2.0,
            400.0,
        ),
        int_input(
            "widthPct",
            msg("indicator.vpvr.widthPct", "Width %"),
            DEFAULT_WIDTH_PCT,
            5.0,
            90.0,
        )
        .tooltip(msg(
            "indicator.vpvr.widthPct.tooltip",
            "Panel width the largest row occupies.",
        )),
        ClassicInput::new(
            "upColor",
            msg("indicator.vpvr.upColor", "Up color"),
            InputType::Color,
            DEFAULT_UP,
        ),
        ClassicInput::new(
            "downColor",
            msg("indicator.vpvr.downColor", "Down color"),
            InputType::Color,
            DEFAULT_DOWN,
        ),
        ClassicInput::new(
            "showPoc",
            msg("indicator.vpvr.showPoc", "Point of control"),
            InputType::Bool,
            true,
        ),
        int_input(
            "valueAreaPct",
            msg("indicator.vpvr.valueAreaPct", "Value area %"),
            DEFAULT_VALUE_AREA_PCT,
            0.0,
            100.0,
        ),
    ];
    NativeIndicatorDescriptor {
        indicator_type: "vpvr".to_string(),
        title: msg("indicator.vpvr.title", "Visible Range Volume Profile"),
        short_title: Some(msg("indicator.vpvr.shortTitle", "VRVP")),
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
        factory: Arc::new(|| Box::<VpvrIndicator>::default()),
    }
}

/// Register the built-in VPVR native indicator (idempotent).
pub fn register_vpvr(registry: &mut NativeIndicatorRegistry) {
    registry.register(vpvr_descriptor());
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::model::Ohlcv;

    #[derive(Default)]
    struct RecordingContext {
        emitted: usize,
        pushed: Vec<Value>,
    }

    impl NativeIndicatorContext for RecordingContext {
        fn id(&self) -> &str {
            "vpvr-1"
        }
        fn symbol(&self) -> &str {
            "TEST"
        }
        fn timeframe(&self) -> &str {
            "1"
        }
        fn live(&self) -> bool {
            true
        }
        fn bars(&self) -> &[Ohlcv] {
            &[]
        }
        fn emit(&mut self, _out: NativeIndicatorOutput) {
            self.emitted += 1;
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
        let data = vpvr_layer_data(&Inputs::new());
        assert_eq!(data.rows, 24);
        assert_eq!(data.width_frac, 0.3);
        assert_eq!(data.up_color, ACCENT);
        assert_eq!(data.down_color, BEARISH);
        assert!(data.show_poc);
        assert_eq!(data.value_area_frac, 0.7);
    }

    #[test]
    fn layer_data_clamps_every_field() {
        let data = vpvr_layer_data(&inputs(&[
            ("rows", InputValue::Number(1000.0)),
            ("widthPct", InputValue::Number(2.0)),
            ("valueAreaPct", InputValue::Number(250.0)),
            ("showPoc", InputValue::Bool(false)),
        ]));
        assert_eq!(data.rows, 400);
        assert_eq!(data.width_frac, 0.05);
        assert_eq!(data.value_area_frac, 1.0);
        assert!(!data.show_poc);
        let low = vpvr_layer_data(&inputs(&[
            ("rows", InputValue::Number(0.0)),
            ("widthPct", InputValue::Number(500.0)),
            ("valueAreaPct", InputValue::Number(-5.0)),
        ]));
        assert_eq!(low.rows, 2);
        assert_eq!(low.width_frac, 0.9);
        assert_eq!(low.value_area_frac, 0.0);
    }

    #[test]
    fn rows_round_to_the_nearest_integer_and_junk_falls_back() {
        let rows = |v: InputValue| vpvr_layer_data(&inputs(&[("rows", v)])).rows;
        assert_eq!(rows(InputValue::Number(12.4)), 12);
        assert_eq!(rows(InputValue::Number(12.5)), 13);
        assert_eq!(rows(InputValue::Number(f64::NAN)), 24);
        assert_eq!(rows(InputValue::Text("50".into())), 24);
    }

    #[test]
    fn only_an_explicit_false_hides_the_point_of_control() {
        let poc = |v: InputValue| vpvr_layer_data(&inputs(&[("showPoc", v)])).show_poc;
        assert!(!poc(InputValue::Bool(false)));
        assert!(poc(InputValue::Bool(true)));
        assert!(poc(InputValue::Number(0.0)));
        assert!(poc(InputValue::Text("false".into())));
    }

    #[test]
    fn the_descriptor_declares_the_original_type() {
        let d = vpvr_descriptor();
        assert_eq!(d.indicator_type, "vpvr");
        assert_eq!(d.pane_hint, PaneHint::Price);
        assert!(d.overlay);
        assert!(matches!(
            &d.short_title,
            Some(crate::i18n::MessageLike::Message(m)) if m.default_message == "VRVP"
        ));
        let keys: Vec<&str> = d.inputs_schema.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "rows",
                "widthPct",
                "upColor",
                "downColor",
                "showPoc",
                "valueAreaPct"
            ]
        );
        let defaults = d.default_inputs();
        assert_eq!(defaults.get("rows"), Some(&InputValue::Number(24.0)));
        assert_eq!(defaults.get("showPoc"), Some(&InputValue::Bool(true)));
        assert_eq!(
            defaults.get("valueAreaPct"),
            Some(&InputValue::Number(70.0))
        );
        assert_eq!(vpvr_layer_data(&defaults), vpvr_layer_data(&Inputs::new()));
    }

    #[test]
    fn lifecycle_pushes_only_after_start() {
        let d = vpvr_descriptor();
        let mut instance = d.create();
        let mut ctx = RecordingContext::default();
        instance.set_inputs(&mut ctx, inputs(&[("rows", InputValue::Number(10.0))]));
        instance.resume(&mut ctx);
        assert!(ctx.pushed.is_empty());
        instance.start(&mut ctx, d.default_inputs());
        assert_eq!(ctx.emitted, 1);
        assert_eq!(
            ctx.pushed,
            [json!({
                "rows": 24, "widthFrac": 0.3, "upColor": ACCENT, "downColor": BEARISH,
                "showPoc": true, "valueAreaFrac": 0.7
            })]
        );
        instance.set_inputs(&mut ctx, inputs(&[("rows", InputValue::Number(10.0))]));
        instance.suspend();
        instance.resume(&mut ctx);
        instance.stop(&mut ctx);
        assert_eq!(ctx.pushed.len(), 3);
        assert_eq!(ctx.pushed[2]["rows"], json!(10));
    }

    #[test]
    fn registering_is_idempotent() {
        let mut registry = NativeIndicatorRegistry::new();
        register_vpvr(&mut registry);
        register_vpvr(&mut registry);
        assert_eq!(registry.types(), ["vpvr"]);
    }
}
