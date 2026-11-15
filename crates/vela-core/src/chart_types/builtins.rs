//! Built-in chart types registered through the SAME public registry a plugin would
//! use: Heikin Ashi is the reference bar-transform type. (Volume/VPVR go through the
//! native indicator registry instead: they are layers over any style, not styles
//! themselves.)

use std::sync::{Arc, OnceLock};

use crate::i18n::msg;
use crate::price_styles::{BarTransform, HeikinAshi};

use super::registry::{ChartTypeDefinition, ChartTypeRegistry};

/// The one Heikin Ashi transform instance, so the orchestrator can compare
/// transforms by identity ([`Arc::ptr_eq`]) across style flips and registries.
pub fn heikin_ashi_transform() -> Arc<dyn BarTransform> {
    static HEIKIN_ASHI: OnceLock<Arc<dyn BarTransform>> = OnceLock::new();
    HEIKIN_ASHI.get_or_init(|| Arc::new(HeikinAshi)).clone()
}

/// Register the built-in chart types (idempotent).
pub fn register_builtin_chart_types(registry: &mut ChartTypeRegistry) {
    let mut heikin = ChartTypeDefinition::new("heikinashi");
    heikin.label = Some(msg("chart.priceStyle.heikinashi", "Heikin Ashi"));
    heikin.bar_transform = Some(heikin_ashi_transform());
    // Averages open, high and low: one value per bar leaves nothing to average.
    heikin.supports = Some(Arc::new(|facts| facts.ohlc != Some(false)));
    registry.register(heikin);
}
