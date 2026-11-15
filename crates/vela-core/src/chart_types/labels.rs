//! The names of the price styles, shared by every picker that lists them.

use crate::i18n::{MessageLike, msg};

use super::registry::ChartTypeRegistry;

/// The built-in styles' names, in picker order. A registered chart type names
/// itself (`label`).
pub fn price_style_labels() -> [(&'static str, MessageLike); 8] {
    [
        ("candles", msg("chart.priceStyle.candles", "Candles")),
        ("bars", msg("chart.priceStyle.bars", "Bars")),
        ("line", msg("chart.priceStyle.line", "Line")),
        (
            "linemarkers",
            msg("chart.priceStyle.linemarkers", "Line with markers"),
        ),
        ("stepline", msg("chart.priceStyle.stepline", "Step line")),
        ("area", msg("chart.priceStyle.area", "Area")),
        ("baseline", msg("chart.priceStyle.baseline", "Baseline")),
        ("columns", msg("chart.priceStyle.columns", "Columns")),
    ]
}

/// The tooltip of a style a picker greys out: the symbol's data cannot be drawn in it.
pub fn price_style_unavailable() -> MessageLike {
    msg(
        "chart.priceStyle.unavailable",
        "Not available for this symbol's data",
    )
}

/// A price style's name: the registered type's label, the built-in name, else the raw id.
pub fn price_style_message(registry: &ChartTypeRegistry, id: &str) -> MessageLike {
    if let Some(label) = registry.get(id).and_then(|d| d.label.clone()) {
        return label;
    }
    price_style_labels()
        .into_iter()
        .find(|(key, _)| *key == id)
        .map(|(_, label)| label)
        .unwrap_or_else(|| MessageLike::from(id.to_string()))
}
