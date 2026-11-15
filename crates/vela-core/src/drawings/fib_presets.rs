//! Ready-made level sets for the Fibonacci retracement. Each keeps the whole
//! ladder of levels (the ones it doesn't use are present but off, so turning
//! one on is a click) and sets which end of the swing level 0 sits on.

use crate::i18n::{MessageLike, msg};
use crate::util::js_number;

use super::level_palette::{FibLevel, LEVEL_AMBER, LevelSpec, fib_levels};

/// The key of a ready-made level set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FibPresetKey {
    Classic,
    Extension,
    Ote,
}

impl FibPresetKey {
    /// The key's wire form (`classic`, `extension`, `ote`).
    pub fn as_str(self) -> &'static str {
        match self {
            FibPresetKey::Classic => "classic",
            FibPresetKey::Extension => "extension",
            FibPresetKey::Ote => "ote",
        }
    }
}

/// A ready-made level set and the direction it is measured in.
#[derive(Clone, Debug, PartialEq)]
pub struct FibPreset {
    pub key: FibPresetKey,
    pub label: MessageLike,
    /// Level 0 on the first anchor (the swing's start) instead of the second.
    pub reverse: bool,
    pub levels: Vec<FibLevel>,
}

/// The standard ladder: retracements, extensions, then targets beyond the swing's end.
const LADDER: [f64; 24] = [
    0.0, 0.236, 0.382, 0.5, 0.618, 0.705, 0.786, 0.886, 1.0, 1.272, 1.414, 1.618, 2.0, 2.272,
    2.414, 2.618, 3.0, 3.272, 3.618, 4.236, 4.618, -0.272, -0.618, -1.0,
];

/// The optimal-trade-entry ladder: the 0.62 to 0.79 entry zone around 0.705,
/// equilibrium at 0.5, and targets measured beyond the swing's end.
const OTE_LADDER: [f64; 24] = [
    0.0, 0.236, 0.382, 0.5, 0.62, 0.705, 0.79, 0.886, 1.0, 1.272, 1.414, 1.618, 2.0, 2.618, 3.0,
    3.618, 4.236, 4.618, -0.27, -0.62, -1.0, -1.5, -2.0, -2.5,
];

/// A ladder of levels with only `on` enabled; `colors` overrides the palette
/// by ratio, keyed by the ratio's text (`String(ratio)`).
pub fn ladder(ratios: &[f64], on: &[f64], colors: &[(&str, &str)]) -> Vec<FibLevel> {
    fib_levels(ratios.iter().map(|&ratio| {
        let key = js_number(ratio);
        let mut spec = LevelSpec::ratio(ratio).enabled(on.contains(&ratio));
        if let Some((_, color)) = colors.iter().find(|(k, _)| *k == key) {
            spec = spec.color(*color);
        }
        spec
    }))
}

/// The classic set: the standard pullback ratios on, the rest of the ladder off.
pub fn classic_levels() -> Vec<FibLevel> {
    ladder(&LADDER, &[0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0], &[])
}

/// Every ready-made set, in toolbar order.
pub fn fib_presets() -> Vec<FibPreset> {
    vec![
        FibPreset {
            key: FibPresetKey::Classic,
            label: msg("drawing.fibPreset.classic", "Classic"),
            reverse: false,
            levels: classic_levels(),
        },
        FibPreset {
            key: FibPresetKey::Extension,
            label: msg("drawing.fibPreset.extension", "Extension"),
            reverse: true,
            levels: ladder(
                &LADDER,
                &[0.0, 1.0, 1.272, 1.414, 1.618, 2.0, 2.618, 3.618, 4.236],
                &[],
            ),
        },
        FibPreset {
            key: FibPresetKey::Ote,
            label: msg("drawing.fibPreset.ote", "OTE"),
            reverse: false,
            // The entry zone reads as one band.
            levels: ladder(
                &OTE_LADDER,
                &[0.0, 0.5, 0.62, 0.705, 0.79, 1.0, -0.27, -0.62],
                &[("0.62", LEVEL_AMBER), ("0.79", LEVEL_AMBER)],
            ),
        },
    ]
}

/// The preset a level set and direction match exactly, or `None` once the
/// user has made them their own.
pub fn match_fib_preset(levels: &[FibLevel], reverse: bool) -> Option<FibPresetKey> {
    let same = |a: &[FibLevel], b: &[FibLevel]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.ratio == y.ratio
                    && x.color == y.color
                    && x.enabled == y.enabled
                    && x.label.as_deref().unwrap_or("") == y.label.as_deref().unwrap_or("")
            })
    };
    fib_presets()
        .into_iter()
        .find(|p| p.reverse == reverse && same(&p.levels, levels))
        .map(|p| p.key)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::drawing::{Drawing, DrawingInit};
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::level_palette::{LEVEL_GREEN, LEVEL_TEAL};
    use crate::drawings::types::fib::{FibLevels, FibRatios, FibRetracement};
    use crate::i18n::source_text;

    fn swing() -> FibRetracement {
        FibRetracement::new(&DrawingInit::on_pane("price").anchors(vec![
            DrawingPoint {
                time: 50.0,
                price: 100.0,
            },
            DrawingPoint {
                time: 150.0,
                price: 150.0,
            },
        ]))
    }

    fn use_preset(d: &mut FibRetracement, key: FibPresetKey) {
        let p = fib_presets().into_iter().find(|p| p.key == key).unwrap();
        let patch = json!({ "levels": p.levels, "reverse": p.reverse });
        d.apply_settings(patch.as_object().unwrap());
    }

    fn price_of(d: &FibRetracement, ratio: f64) -> Option<f64> {
        let proj = crate::drawings::projector::LinearProjector::sample();
        d.level_lines(&proj)?
            .iter()
            .find(|l| l.ratio == ratio)
            .map(|l| l.price)
    }

    #[test]
    fn the_sets_are_classic_extension_and_ote_in_that_order() {
        let labels: Vec<_> = fib_presets()
            .iter()
            .map(|p| source_text(&p.label))
            .collect();
        assert_eq!(labels, ["Classic", "Extension", "OTE"]);
        let keys: Vec<_> = fib_presets().iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["classic", "extension", "ote"]);
    }

    #[test]
    fn every_set_keeps_a_long_ladder_with_unused_levels_off() {
        for p in fib_presets() {
            assert!(p.levels.len() >= 24, "{:?}", p.key);
            assert!(p.levels.iter().any(|l| !l.enabled), "{:?}", p.key);
        }
    }

    #[test]
    fn ladder_colors_by_ratio_text_and_enables_only_the_listed() {
        let levels = ladder(&[0.5, 0.62, 7.0], &[0.62], &[("0.62", LEVEL_AMBER)]);
        assert_eq!(levels[0].color, LEVEL_GREEN);
        assert!(!levels[0].enabled);
        assert_eq!(levels[1].color, LEVEL_AMBER);
        assert!(levels[1].enabled);
        assert!(!levels[2].enabled);
        let classic = classic_levels();
        assert_eq!(classic[4].ratio, 0.618);
        assert_eq!(classic[4].color, LEVEL_TEAL);
    }

    #[test]
    fn a_new_retracement_starts_on_classic() {
        let d = swing();
        assert_eq!(
            match_fib_preset(&d.fib_core().levels, d.reverse),
            Some(FibPresetKey::Classic)
        );
        let on: Vec<f64> = d
            .fib_core()
            .levels
            .iter()
            .filter(|l| l.enabled)
            .map(|l| l.ratio)
            .collect();
        assert_eq!(on, [0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0]);
        assert_eq!(price_of(&d, 0.0), Some(150.0));
        assert_eq!(price_of(&d, 0.5), Some(125.0));
        assert_eq!(price_of(&d, 1.0), Some(100.0));
    }

    #[test]
    fn extension_projects_targets_from_the_swing_low() {
        let mut d = swing();
        use_preset(&mut d, FibPresetKey::Extension);
        assert!(d.reverse);
        assert_eq!(price_of(&d, 0.0), Some(100.0));
        assert_eq!(price_of(&d, 1.0), Some(150.0));
        assert!((price_of(&d, 1.618).unwrap() - 180.9).abs() < 1e-9);
        assert!((price_of(&d, 2.618).unwrap() - 230.9).abs() < 1e-9);
    }

    #[test]
    fn ote_marks_the_entry_zone_and_targets_above() {
        let mut d = swing();
        use_preset(&mut d, FibPresetKey::Ote);
        let mut on: Vec<f64> = d
            .fib_core()
            .levels
            .iter()
            .filter(|l| l.enabled)
            .map(|l| l.ratio)
            .collect();
        on.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(on, [-0.62, -0.27, 0.0, 0.5, 0.62, 0.705, 0.79, 1.0]);
        assert_eq!(price_of(&d, 0.5), Some(125.0));
        assert!((price_of(&d, 0.705).unwrap() - 114.75).abs() < 1e-9);
        assert!((price_of(&d, -0.27).unwrap() - 163.5).abs() < 1e-9);
        assert!((price_of(&d, -0.62).unwrap() - 181.0).abs() < 1e-9);
        let zone: Vec<&str> = d
            .fib_core()
            .levels
            .iter()
            .filter(|l| [0.62, 0.705, 0.79].contains(&l.ratio))
            .map(|l| l.color.as_str())
            .collect();
        assert!(zone.iter().all(|c| *c == zone[0]));
    }

    #[test]
    fn editing_a_level_makes_the_set_your_own_and_choosing_again_restores_it() {
        let mut d = swing();
        let patch = json!({ "levels.5.enabled": true });
        d.apply_settings(patch.as_object().unwrap());
        assert_eq!(match_fib_preset(&d.fib_core().levels, d.reverse), None);
        use_preset(&mut d, FibPresetKey::Classic);
        assert_eq!(
            match_fib_preset(&d.fib_core().levels, d.reverse),
            Some(FibPresetKey::Classic)
        );
        // The same levels in the other direction match no set.
        assert_eq!(match_fib_preset(&d.fib_core().levels, true), None);
    }

    #[test]
    fn a_missing_label_matches_an_empty_one() {
        let mut levels = classic_levels();
        levels[0].label = Some(String::new());
        assert_eq!(
            match_fib_preset(&levels, false),
            Some(FibPresetKey::Classic)
        );
        levels[0].label = Some("x".to_string());
        assert_eq!(match_fib_preset(&levels, false), None);
    }
}
