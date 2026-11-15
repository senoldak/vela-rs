//! The classic indicator catalog: the standard technical-analysis studies
//! every charting host expects (moving averages, oscillators, bands, volume
//! studies), each a pure compute over the chart's own bars.

pub mod averages;
pub mod bands;
pub mod define;
pub mod labels;
pub mod math;
pub mod oscillators;
pub mod overlays;
pub mod shared;
pub mod trend;
pub mod volatility;
pub mod volume;

pub use define::{
    ClassicBand, ClassicCompute, ClassicIndicatorSpec, ClassicInput, ClassicLevel, ClassicOutput,
    ClassicPlot, ClassicPolyline, InputLabels, classic_descriptor, classic_native_output,
    localize_input,
};

use super::NativeIndicatorRegistry;

/// Every catalog entry, in the original's order.
pub fn classic_specs() -> Vec<ClassicIndicatorSpec> {
    let mut specs = averages::average_specs();
    specs.extend(bands::band_specs());
    specs.extend(oscillators::oscillator_specs());
    specs.extend(trend::trend_specs());
    specs.extend(volatility::volatility_specs());
    specs.extend(volume::volume_specs());
    specs.extend(overlays::overlay_specs());
    specs
}

/// Register the classic indicator catalog (idempotent).
pub fn register_classic_indicators(registry: &mut NativeIndicatorRegistry) {
    for spec in classic_specs() {
        registry.register(classic_descriptor(&spec));
    }
}
