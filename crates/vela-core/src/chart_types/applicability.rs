//! Which price styles can draw the chart's symbol. A style drawn from open, high and
//! low has nothing to show on a series with one value per bar (`SymbolInfo.ohlc ==
//! Some(false)`): the chart then shows a close-based style in its place, without
//! touching the chosen one, and the style pickers grey it out. Unknown facts grey
//! nothing.

use crate::ports::market_data::SymbolFacts;

use super::registry::ChartTypeRegistry;

/// The built-in styles drawn from open, high and low. Every other built-in draws the
/// close and applies to any symbol; a registered type states its own rule (`supports`).
const OHLC_BUILTINS: [&str; 2] = ["candles", "bars"];

/// The styles shown in place of one the symbol cannot draw, in order of preference:
/// the first that exists and applies wins.
pub const PRICE_STYLE_FALLBACKS: [&str; 2] = ["linemarkers", "line"];

/// Whether `style` can draw the symbol `facts` describe. `None` facts (not known
/// yet, or a source that states none) mean every style applies. A registered type
/// answers through its `supports` predicate.
///
/// The original counts a `supports` that throws as supporting; a Rust predicate
/// that panics is a bug in the predicate and is not caught here.
pub fn price_style_applies(
    registry: &ChartTypeRegistry,
    style: &str,
    facts: Option<&SymbolFacts>,
) -> bool {
    let Some(facts) = facts else {
        return true;
    };
    if let Some(supports) = registry.get(style).and_then(|d| d.supports.as_ref()) {
        return supports(facts);
    }
    !(facts.ohlc == Some(false) && OHLC_BUILTINS.contains(&style))
}

/// The style a chart set to `style` draws for the symbol `facts` describe: `style`
/// itself when it applies, else the first of [`PRICE_STYLE_FALLBACKS`] among `known`
/// (the style ids that exist right now) that applies, else `style` (nothing better
/// to offer).
pub fn shown_price_style<S: AsRef<str>>(
    registry: &ChartTypeRegistry,
    style: &str,
    facts: Option<&SymbolFacts>,
    known: &[S],
) -> String {
    if price_style_applies(registry, style, facts) {
        return style.to_string();
    }
    for id in PRICE_STYLE_FALLBACKS {
        if known.iter().any(|k| k.as_ref() == id) && price_style_applies(registry, id, facts) {
            return id.to_string();
        }
    }
    style.to_string()
}
