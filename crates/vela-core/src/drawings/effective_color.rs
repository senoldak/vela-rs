//! The color a drawing's body is ACTUALLY filled with.

use crate::palette::{INVALID, VALID};

use super::drawing::Drawing;
use super::types::annotations::is_callout;
use super::types::patterns::pattern_fill_info;

/// Semantic validity-tint colors, shared by a painter (body wash, name badge, leg ratios).
pub const VALID_FILL: &str = VALID;
pub const INVALID_FILL: &str = INVALID;

/// The minimal theme slice the color resolution needs.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveColorTheme {
    pub background: String,
}

/// The single source of truth shared by a renderer (so it paints it) and a
/// settings popup (so the Fill swatch reflects it, never a stale default).
/// Returns `None` when no single user-editable body fill applies: plain
/// unfilled shapes, or direction/zone-tinted and per-level-filled tools.
///
/// Precedence mirrors the painter: an explicit `style.fill_color` always
/// wins; otherwise a pattern falls back to the harmonic validity tint (green
/// valid / red invalid) or, for non-validated patterns, the line color; a
/// callout falls back to the chart background.
///
/// This returns the COLOR only. A renderer's per-type opacity is a separate
/// painter concern and must NOT be baked in here, or repeated round-trips
/// through a picker would compound it.
pub fn effective_fill_color(d: &dyn Drawing, theme: &EffectiveColorTheme) -> Option<String> {
    let style = &d.base().style;
    if let Some(pattern) = pattern_fill_info(d) {
        if !pattern.has_fill_triangles {
            return None; // plain ABCD / Elliott / Head & Shoulders paint no body
        }
        return Some(
            style
                .fill_color
                .clone()
                .unwrap_or_else(|| match pattern.valid {
                    None => style.line_color.clone(),
                    Some(true) => VALID_FILL.to_string(),
                    Some(false) => INVALID_FILL.to_string(),
                }),
        );
    }
    if is_callout(d) {
        return Some(
            style
                .fill_color
                .clone()
                .unwrap_or_else(|| theme.background.clone()),
        );
    }
    style.fill_color.clone()
}
