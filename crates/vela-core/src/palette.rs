//! The semantic palette: every fixed color Vela ships that is not part of a
//! swappable chart theme. These are meaning constants (an accent stays the
//! accent whichever theme is active).

/// Selection/menu accent: active entries, "native" badges, selected controls.
pub const ACCENT: &str = "#2962ff";
/// The lighter brand blue: "on" affordances and the default drawing color.
pub const ACCENT_BRIGHT: &str = "#38c0fd";
/// Bullish/bearish reference pair, reused wherever a fixed directional color is needed.
pub const BULLISH: &str = "#089981";
pub const BEARISH: &str = "#f23645";
/// Neutral gray for de-emphasized geometry.
pub const NEUTRAL: &str = "#787b86";
/// Attention amber: favorited items.
pub const HIGHLIGHT: &str = "#e0b400";
/// Warm accent used by the categorical palette and warning-ish marks.
pub const WARNING: &str = "#ff9800";
/// Soft informational blue: statistical overlays (regression, VWAP).
pub const INFO: &str = "#5b9cf6";
/// Highlighter ink, always drawn translucent.
pub const MARKER: &str = "#ff5d00";
/// Validity tints: a pattern that satisfies its rules vs one that does not.
pub const VALID: &str = "#0ecb81";
pub const INVALID: &str = "#f6465d";
/// Market-session states: pre-market, post-market, closed/holiday.
pub const SESSION_PRE: &str = "#f97316";
pub const SESSION_POST: &str = "#0ea5e9";
pub const SESSION_OFF: &str = "#9ca3af";
/// Default color of a plain line/area series.
pub const SERIES_LINE: &str = "#3b82f6";
/// Crosshair ink.
pub const CROSSHAIR: &str = "#9aa0ad";
/// Fixed slate plates for canvas badges: `SLATE_DEEP` is the plate, `SLATE` its border.
pub const SLATE_DEEP: &str = "#1e293b";
pub const SLATE: &str = "#475569";
/// The crosshair's axis chips.
pub const CHIP_PLATE: &str = "#595959";
/// Strategy trade markers: entry arrows per position side, and the exits' shared violet.
pub const TRADE_LONG: &str = ACCENT;
pub const TRADE_SHORT: &str = BEARISH;
pub const TRADE_EXIT: &str = "#d500f9";

/// Categorical hues for auto-assigned colors, ordered for adjacent-hue contrast.
pub const CATEGORICAL: [&str; 8] = [
    ACCENT, BULLISH, BEARISH, WARNING, "#7e57c2", "#26a69a", INFO, "#e573b5",
];

/// Pick a stable categorical color for a string key (same key ⇒ same color).
pub fn categorical_color(key: &str) -> &'static str {
    // The original hashes UTF-16 code units; do the same so non-ASCII keys agree.
    let mut h: u32 = 0;
    for unit in key.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(unit));
    }
    CATEGORICAL[(h as usize) % CATEGORICAL.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_same_color() {
        assert_eq!(categorical_color("AAPL"), categorical_color("AAPL"));
        assert_eq!(categorical_color(""), ACCENT);
    }
}
