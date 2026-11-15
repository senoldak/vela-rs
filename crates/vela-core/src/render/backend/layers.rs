//! The public RENDERER-LAYER registry: the native renderer's extension seam for custom
//! canvas layers (the counterpart of the chart-type registry on the data side). A layer
//! owns ONE transparent canvas stacked into the chart's layer pile and is repainted from
//! the shared paint cycle with the chart's bars, its own native-data channel, and the pane
//! geometry.
//!
//! The original keeps one process-wide registry and renderers pick it up when they mount.
//! Here the registry is a value, like [`ChartTypeRegistry`](crate::chart_types::ChartTypeRegistry):
//! register before a chart is constructed and hand it to the renderer. The layer's `id`
//! doubles as its data channel: the core pushes `setNativeData(id, ...)` payloads (e.g.
//! from a chart type's data engine via `host.pushData`) and `setNativeData(id + "-pending",
//! ...)` loading ranges; both arrive in the layer's render args every frame.
//!
//! A layer instance's `mount` receives the canvas the renderer created, as a shared
//! [`Canvas2d`]: keep it and paint into it.

use std::rc::Rc;

use serde_json::{Map, Value};

use crate::i18n::Translator;
use crate::model::Ohlcv;
use crate::ports::chart_renderer::VelaTheme;
use crate::render::canvas::Canvas2d;
use crate::render::core::{CoordinateSystem, PaneBounds, PriceScale};

/// A plot-relative pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorPoint {
    pub x: f64,
    pub y: f64,
}

/// Everything a layer needs to paint one frame.
pub struct RendererLayerArgs<'a> {
    /// The chart's CURRENT view bars (post bar-transform).
    pub bars: &'a [Ohlcv],
    /// The layer's channel payload (last `setNativeData(id, ...)` push; `None` before the
    /// first).
    pub data: Option<&'a Value>,
    /// The type's SDK settings values (`<id>-settings` channel; empty before any edit).
    pub settings: &'a Map<String, Value>,
    /// Time ranges still loading (`<id>-pending` channel): skeleton and reveal UIs.
    pub pending: &'a [(f64, f64)],
    pub coords: &'a CoordinateSystem,
    /// The scale and bounds of the pane the layer paints on: the pane of the native
    /// indicator that owns this channel (its type equals the layer id), else the price
    /// pane, so a layer-backed indicator moved to its own pane takes its layer along.
    pub scale: PriceScale,
    pub bounds: PaneBounds,
    pub theme: &'a VelaTheme,
    /// The active price style: layers tied to a chart type gate their visibility on it.
    pub price_style: &'a str,
    /// Frame clock (ms) for pulses and fades; monotonic within a session.
    pub now_ms: f64,
    /// Plot-relative pointer position, or `None` when the pointer is off the plot. Layers
    /// that hover-test set `repaint_on_cursor` on their definition so pointer moves
    /// repaint them (only data-tier frames repaint layers otherwise).
    pub cursor: Option<CursorPoint>,
    /// The chart's translator: `i18n.t(&msg(...))` for text a layer paints, and
    /// `i18n.format()` for its numbers and dates, in the chart's locale.
    pub i18n: &'a dyn Translator,
}

/// How a layer dims or slims the BASE painting under it (see
/// [`RendererLayerInstance::modulate_base`]). Omitted fields keep their defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BasePaintingModulation {
    /// Candle body width multiplier, `(0, 1]` (1 = full width).
    pub candle_body_scale: Option<f64>,
    /// Candle body-fill opacity, `[0, 1]` (wick and border keep their own opacity).
    pub candle_body_alpha: Option<f64>,
    /// Gridline opacity, `[0, 1]` (fade the grid as a reveal-under layer opens).
    pub grid_alpha: Option<f64>,
}

/// Fold one layer's modulation into the running request. Each field keeps the stronger
/// (smaller) of the two; omitted stays omitted. `None` is no opinion.
pub fn fold_base_modulation(
    acc: Option<BasePaintingModulation>,
    next: Option<BasePaintingModulation>,
) -> Option<BasePaintingModulation> {
    let Some(next) = next else {
        return acc;
    };
    let Some(acc) = acc else {
        return Some(next);
    };
    Some(BasePaintingModulation {
        candle_body_scale: min_opt(acc.candle_body_scale, next.candle_body_scale),
        candle_body_alpha: min_opt(acc.candle_body_alpha, next.candle_body_alpha),
        grid_alpha: min_opt(acc.grid_alpha, next.grid_alpha),
    })
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(crate::indicators::classics::math::js_min(a, b)),
    }
}

/// One live layer instance (per mounted renderer).
pub trait RendererLayerInstance {
    /// The renderer created (and owns) this transparent canvas: keep the reference, paint
    /// into it.
    fn mount(&mut self, canvas: Rc<dyn Canvas2d>);

    /// Paint one frame. Always clear and redraw your own canvas: the renderer never
    /// clears it for you.
    fn render(&mut self, args: &RendererLayerArgs);

    /// Return true while the layer needs CONTINUOUS frames (a pulse or fade): keeps the
    /// animator alive.
    fn animating(&self) -> bool {
        false
    }

    /// How the base painting (candles, grid) should be dimmed or slimmed under this layer
    /// for the CURRENT frame: a gradual counterpart of the chart type's all-or-nothing
    /// `basePainting: 'none'`. Called after `render` on every mounted layer (chart-type and
    /// overlay alike); returning `None` (the default) is no opinion. When several layers
    /// speak, each field keeps the strongest (smallest) request. Values are clamped by the
    /// renderer.
    fn modulate_base(&mut self, _args: &RendererLayerArgs) -> Option<BasePaintingModulation> {
        None
    }

    /// The renderer unmounted: release everything (the canvas itself is removed by the
    /// renderer).
    fn destroy(&mut self) {}
}

/// Where a layer stacks relative to the data canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LayerPlacement {
    /// Behind the candles (reveal-under styles).
    BelowData,
    /// Over the candles, under the chrome and axes.
    #[default]
    AboveData,
}

impl LayerPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerPlacement::BelowData => "below-data",
            LayerPlacement::AboveData => "above-data",
        }
    }
}

/// Builds a layer instance.
pub type LayerFactory = Rc<dyn Fn() -> Box<dyn RendererLayerInstance>>;

/// One registered layer kind.
#[derive(Clone)]
pub struct RendererLayerDefinition {
    /// The layer id: also its native-data channel (`setNativeData(id, ...)` and
    /// `id + "-pending"`).
    pub id: String,
    /// Stacking. A layer owned by a native indicator (its type equals the layer id)
    /// follows that indicator's z key in the pane stacking instead (`seriesOrder` / the
    /// object tree), mounting in front. Either way the gridlines stay BELOW every layer
    /// (they live on the backdrop canvas at the bottom of the pile): no stacking puts a
    /// layer behind the grid. `None` is [`LayerPlacement::AboveData`].
    pub placement: Option<LayerPlacement>,
    /// Repaint this layer when the pointer moves (hover hit-testing UIs). Off by default:
    /// pointer moves normally repaint only the crosshair overlay, not the layers.
    pub repaint_on_cursor: Option<bool>,
    /// Instance factory: called once per mounted renderer, plus once per mounted instance
    /// of a `multiInstance` native indicator whose type equals this id (each instance
    /// paints through its own layer instance and canvas, fed by its own channel).
    pub create: LayerFactory,
}

impl RendererLayerDefinition {
    pub fn new(
        id: impl Into<String>,
        create: impl Fn() -> Box<dyn RendererLayerInstance> + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            placement: None,
            repaint_on_cursor: None,
            create: Rc::new(create),
        }
    }

    /// The effective placement.
    pub fn placement(&self) -> LayerPlacement {
        self.placement.unwrap_or_default()
    }
}

impl std::fmt::Debug for RendererLayerDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererLayerDefinition")
            .field("id", &self.id)
            .field("placement", &self.placement)
            .field("repaint_on_cursor", &self.repaint_on_cursor)
            .finish_non_exhaustive()
    }
}

/// The set of registered renderer layers, in registration order.
#[derive(Clone, Debug, Default)]
pub struct LayerRegistry {
    layers: Vec<RendererLayerDefinition>,
}

impl LayerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a renderer layer. A replaced layer keeps its position.
    /// Renderers mounted AFTER registration pick it up.
    pub fn register(&mut self, def: RendererLayerDefinition) {
        match self.layers.iter_mut().find(|d| d.id == def.id) {
            Some(slot) => *slot = def,
            None => self.layers.push(def),
        }
    }

    pub fn unregister(&mut self, id: &str) {
        self.layers.retain(|d| d.id != id);
    }

    /// The definition registered under `id`.
    pub fn get(&self, id: &str) -> Option<&RendererLayerDefinition> {
        self.layers.iter().find(|d| d.id == id)
    }

    /// Every registered layer definition (registration order).
    pub fn layers(&self) -> &[RendererLayerDefinition] {
        &self.layers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::english_translator;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    struct Noop;

    impl RendererLayerInstance for Noop {
        fn mount(&mut self, _: Rc<dyn Canvas2d>) {}
        fn render(&mut self, _: &RendererLayerArgs) {}
    }

    fn noop() -> Box<dyn RendererLayerInstance> {
        Box::new(Noop)
    }

    #[test]
    fn register_replace_by_id_unregister_list() {
        let mut registry = LayerRegistry::new();
        registry.register(RendererLayerDefinition::new("demo", noop));
        assert!(registry.layers().iter().any(|d| d.id == "demo"));
        registry.register(RendererLayerDefinition {
            placement: Some(LayerPlacement::BelowData),
            ..RendererLayerDefinition::new("demo", noop)
        });
        // Last wins.
        assert_eq!(
            registry.get("demo").map(|d| d.placement()),
            Some(LayerPlacement::BelowData)
        );
        assert_eq!(registry.layers().len(), 1);
        registry.unregister("demo");
        assert!(!registry.layers().iter().any(|d| d.id == "demo"));
        registry.unregister("never-registered");
    }

    #[test]
    fn a_replaced_layer_keeps_its_position_and_placement_defaults_above_the_data() {
        let mut registry = LayerRegistry::new();
        registry.register(RendererLayerDefinition::new("a", noop));
        registry.register(RendererLayerDefinition::new("b", noop));
        registry.register(RendererLayerDefinition::new("a", noop));
        let ids: Vec<&str> = registry.layers().iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(
            registry.get("a").map(|d| d.placement()),
            Some(LayerPlacement::AboveData)
        );
        assert_eq!(LayerPlacement::BelowData.as_str(), "below-data");
        assert_eq!(LayerPlacement::AboveData.as_str(), "above-data");
    }

    #[test]
    fn carries_the_cursor_repaint_opt_in_on_the_definition() {
        let mut registry = LayerRegistry::new();
        registry.register(RendererLayerDefinition {
            repaint_on_cursor: Some(true),
            ..RendererLayerDefinition::new("demo", noop)
        });
        assert_eq!(
            registry.get("demo").and_then(|d| d.repaint_on_cursor),
            Some(true)
        );
    }

    struct Modulating;

    impl RendererLayerInstance for Modulating {
        fn mount(&mut self, _: Rc<dyn Canvas2d>) {}
        fn render(&mut self, args: &RendererLayerArgs) {
            let _ = args.cursor; // hover hit-testing input
        }
        fn modulate_base(&mut self, args: &RendererLayerArgs) -> Option<BasePaintingModulation> {
            (args.price_style == "demo").then_some(BasePaintingModulation {
                candle_body_scale: Some(0.07),
                grid_alpha: Some(0.0),
                ..BasePaintingModulation::default()
            })
        }
    }

    fn args<'a>(
        price_style: &'a str,
        coords: &'a CoordinateSystem,
        theme: &'a VelaTheme,
        settings: &'a Map<String, Value>,
    ) -> RendererLayerArgs<'a> {
        RendererLayerArgs {
            bars: &[],
            data: None,
            settings,
            pending: &[],
            coords,
            scale: PriceScale::new(0.0, 1.0),
            bounds: PaneBounds {
                top: 0.0,
                height: 100.0,
            },
            theme,
            price_style,
            now_ms: 0.0,
            cursor: None,
            i18n: english_translator(),
        }
    }

    #[test]
    fn accepts_a_base_painting_modulating_cursor_reading_instance() {
        let mut registry = LayerRegistry::new();
        registry.register(RendererLayerDefinition::new("demo", || {
            Box::new(Modulating)
        }));
        let coords = CoordinateSystem::new();
        let (theme, settings) = (json!({}), Map::new());
        let mut instance = (registry.get("demo").unwrap().create)();
        instance.mount(Rc::new(RecordingCanvas::new()));
        instance.render(&args("demo", &coords, &theme, &settings));
        assert_eq!(
            instance.modulate_base(&args("demo", &coords, &theme, &settings)),
            Some(BasePaintingModulation {
                candle_body_scale: Some(0.07),
                candle_body_alpha: None,
                grid_alpha: Some(0.0),
            })
        );
        assert_eq!(
            instance.modulate_base(&args("candles", &coords, &theme, &settings)),
            None
        );
        assert!(!instance.animating());
        instance.destroy();
    }

    #[test]
    fn a_factory_makes_an_independent_instance_each_call() {
        let def = RendererLayerDefinition::new("demo", noop);
        let _a = (def.create)();
        let _b = (def.create)();
    }

    fn modulation(
        scale: Option<f64>,
        alpha: Option<f64>,
        grid: Option<f64>,
    ) -> BasePaintingModulation {
        BasePaintingModulation {
            candle_body_scale: scale,
            candle_body_alpha: alpha,
            grid_alpha: grid,
        }
    }

    #[test]
    fn fold_null_is_no_opinion_the_first_speaker_wins_the_field_later_speakers_keep_the_smaller() {
        assert_eq!(fold_base_modulation(None, None), None);
        assert_eq!(
            fold_base_modulation(None, Some(modulation(Some(0.07), None, None))),
            Some(modulation(Some(0.07), None, None))
        );
        assert_eq!(
            fold_base_modulation(
                Some(modulation(Some(0.07), Some(1.0), None)),
                Some(modulation(Some(0.5), None, Some(0.0)))
            ),
            Some(modulation(Some(0.07), Some(1.0), Some(0.0)))
        );
        // A later null does not wipe the running request.
        assert_eq!(
            fold_base_modulation(Some(modulation(Some(0.07), None, None)), None),
            Some(modulation(Some(0.07), None, None))
        );
    }

    #[test]
    fn fold_keeps_the_stronger_value_in_either_order() {
        let low = modulation(Some(0.1), Some(0.2), Some(0.3));
        let high = modulation(Some(0.9), Some(0.8), Some(0.7));
        assert_eq!(fold_base_modulation(Some(low), Some(high)), Some(low));
        assert_eq!(fold_base_modulation(Some(high), Some(low)), Some(low));
    }
}
