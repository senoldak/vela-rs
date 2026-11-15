//! The LEVEL PALETTE: the one color convention every subdivided tool paints
//! its levels with, so the 0.618 of a retracement, a fan, an arc set and a
//! Gann box are all the same color.
//!
//! Two ways to color a level, matching the two kinds of tool:
//! - BY RATIO ([`level_color`]): Fibonacci-style tools, where a ratio has a
//!   conventional color a trader recognizes, and the anchor ratios (0, 1, 2,
//!   3) read as neutral bounds.
//! - BY POSITION ([`cycle_color`]): sequence tools (Gann fan angles, Mach
//!   waves), where the level only needs to differ from its neighbours.

use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL, WARNING};

use super::drawing::EditableLevel;

/// One configurable Fibonacci entry: a ratio (or sequence index) with color / enabled / label.
pub type FibLevel = EditableLevel;

/// Boundary levels (0, 1, 2, 3): the frame of the construction, not a signal.
pub const LEVEL_ANCHOR: &str = NEUTRAL;
pub const LEVEL_RED: &str = BEARISH;
pub const LEVEL_ORANGE: &str = WARNING;
pub const LEVEL_GREEN: &str = "#4caf50";
pub const LEVEL_TEAL: &str = BULLISH;
pub const LEVEL_BLUE: &str = INFO;
pub const LEVEL_PURPLE: &str = "#9c27b0";
pub const LEVEL_PINK: &str = "#e91e63";
pub const LEVEL_AQUA: &str = "#26a69a";
pub const LEVEL_VIOLET: &str = "#ab47bc";
pub const LEVEL_CORAL: &str = "#ef5350";
pub const LEVEL_AMBER: &str = "#ffb74d";
pub const LEVEL_CYAN: &str = "#38c0fd";
/// The 1/1 diagonal of a Gann construction: chart ink, deliberately not a hue.
pub const LEVEL_UNITY: &str = "#b2b5be";

/// The distinguish-by-position sequence, ordered so neighbours contrast.
pub const LEVEL_CYCLE: [&str; 12] = [
    LEVEL_CYAN,
    LEVEL_BLUE,
    LEVEL_TEAL,
    LEVEL_GREEN,
    LEVEL_ORANGE,
    LEVEL_RED,
    LEVEL_PINK,
    LEVEL_PURPLE,
    LEVEL_ANCHOR,
    LEVEL_AQUA,
    LEVEL_VIOLET,
    LEVEL_CORAL,
];

/// The nth color of [`LEVEL_CYCLE`] (wraps, negative indices included).
pub fn cycle_color(index: i64) -> &'static str {
    LEVEL_CYCLE[index.rem_euclid(LEVEL_CYCLE.len() as i64) as usize]
}

/// The conventional color of each Fibonacci-family ratio.
const RATIO_COLORS: [(f64, &str); 36] = [
    (0.0, LEVEL_ANCHOR),
    (0.125, LEVEL_RED),
    (0.236, LEVEL_RED),
    (0.25, LEVEL_RED),
    (0.382, LEVEL_ORANGE),
    (0.5, LEVEL_GREEN),
    (0.618, LEVEL_TEAL),
    (0.75, LEVEL_BLUE),
    (0.786, LEVEL_BLUE),
    (1.0, LEVEL_ANCHOR),
    (0.705, LEVEL_AMBER),
    (0.886, LEVEL_PURPLE),
    (1.272, LEVEL_BLUE),
    (1.382, LEVEL_ORANGE),
    (1.414, LEVEL_AQUA),
    (1.618, LEVEL_RED),
    (2.0, LEVEL_ANCHOR),
    (2.272, LEVEL_VIOLET),
    (2.382, LEVEL_ORANGE),
    (2.414, LEVEL_PINK),
    (2.618, LEVEL_ORANGE),
    (3.0, LEVEL_ANCHOR),
    (3.272, LEVEL_CORAL),
    (3.618, LEVEL_PURPLE),
    (4.236, LEVEL_GREEN),
    (4.618, LEVEL_PINK),
    // Beyond the swing's end (targets): blues, deepening with distance.
    (-0.27, LEVEL_CYAN),
    (-0.272, LEVEL_CYAN),
    (-0.62, LEVEL_BLUE),
    (-0.618, LEVEL_BLUE),
    (-1.0, LEVEL_AQUA),
    (-1.5, LEVEL_VIOLET),
    (-2.0, LEVEL_PURPLE),
    (-2.5, LEVEL_PINK),
    (6.854, LEVEL_TEAL),
    (11.09, LEVEL_BLUE),
];

/// The convention's color for a ratio. Unknown ratios fall back to their
/// position in the cycle, so a tool with a bespoke ratio set still gets
/// distinguishable levels.
pub fn level_color(ratio: f64, index: usize) -> &'static str {
    RATIO_COLORS
        .iter()
        .find(|(r, _)| *r == ratio)
        .map(|(_, c)| *c)
        .unwrap_or_else(|| cycle_color(index as i64))
}

/// One entry in a tool's default level set: a bare ratio takes the
/// convention's color and is enabled; the other fields override that for the
/// tool only.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelSpec {
    pub ratio: f64,
    pub color: Option<String>,
    pub enabled: bool,
    pub label: Option<String>,
}

impl LevelSpec {
    /// An enabled level in the convention's color.
    pub fn ratio(ratio: f64) -> Self {
        Self {
            ratio,
            color: None,
            enabled: true,
            label: None,
        }
    }

    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl From<f64> for LevelSpec {
    fn from(ratio: f64) -> Self {
        Self::ratio(ratio)
    }
}

/// Build a tool's default levels from its specs, colored by the shared convention.
pub fn fib_levels(specs: impl IntoIterator<Item = LevelSpec>) -> Vec<FibLevel> {
    specs
        .into_iter()
        .enumerate()
        .map(|(i, s)| FibLevel {
            ratio: s.ratio,
            color: s
                .color
                .unwrap_or_else(|| level_color(s.ratio, i).to_string()),
            enabled: s.enabled,
            label: s.label.filter(|l| !l.is_empty()),
        })
        .collect()
}

/// Build a tool's default levels from bare ratios, all enabled.
pub fn fib_levels_of(ratios: &[f64]) -> Vec<FibLevel> {
    fib_levels(ratios.iter().map(|&r| LevelSpec::ratio(r)))
}

/// Build a sequence tool's levels, colored by position (1-based ratios and
/// labels); the first `enabled_count` are on.
pub fn cycle_levels(count: usize, enabled_count: usize) -> Vec<FibLevel> {
    (0..count)
        .map(|i| FibLevel {
            ratio: (i + 1) as f64,
            color: cycle_color(i as i64).to_string(),
            enabled: i < enabled_count,
            label: Some((i + 1).to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_take_their_conventional_color() {
        assert_eq!(level_color(0.618, 3), LEVEL_TEAL);
        assert_eq!(level_color(1.0, 0), LEVEL_ANCHOR);
        assert_eq!(level_color(11.09, 0), LEVEL_BLUE);
        assert_eq!(level_color(0.333, 2), LEVEL_CYCLE[2]);
    }

    #[test]
    fn the_cycle_wraps_both_ways() {
        assert_eq!(cycle_color(12), LEVEL_CYAN);
        assert_eq!(cycle_color(-1), LEVEL_CORAL);
    }

    #[test]
    fn level_builders_fill_color_and_labels() {
        let levels = fib_levels([
            LevelSpec::ratio(0.5),
            LevelSpec::ratio(0.62).color(LEVEL_AMBER).enabled(false),
            LevelSpec::ratio(7.0).label(""),
        ]);
        assert_eq!(levels[0].color, LEVEL_GREEN);
        assert!(levels[0].enabled);
        assert_eq!(levels[1].color, LEVEL_AMBER);
        assert!(!levels[1].enabled);
        assert_eq!(levels[2].color, LEVEL_CYCLE[2]);
        assert_eq!(levels[2].label, None);
        let seq = cycle_levels(3, 2);
        assert_eq!(
            seq.iter().map(|l| l.ratio).collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0]
        );
        assert_eq!(seq[2].label.as_deref(), Some("3"));
        assert!(!seq[2].enabled);
    }
}
