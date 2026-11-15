//! Directional and trend-strength studies.

use crate::i18n::msg;
use crate::model::LineStyle;
use crate::palette::{BEARISH, BULLISH, NEUTRAL};

use super::define::{
    ClassicIndicatorSpec, ClassicLevel, ClassicOutput, ClassicPlot, len, num, text,
};
use super::math::{
    bars_since_highest, bars_since_lowest, highs, lows, map, rma, sum, true_range, zip,
};
use super::shared::{MAX_LENGTH, color_input, float_input, length_input, transp};

fn aroon() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "aroon",
        msg("indicator.aroon.title", "Aroon"),
        false,
        vec![
            length_input(
                14.0,
                "length",
                msg("indicator.aroon.length", "Length"),
                MAX_LENGTH,
                Some(msg(
                    "indicator.aroon.length.tooltip",
                    "Lookback length used to locate the highest high and lowest low.",
                )),
            ),
            color_input(
                BULLISH,
                msg("indicator.aroon.upColor", "Aroon Up"),
                "upColor",
                Some(msg(
                    "indicator.aroon.upColor.tooltip",
                    "Color of the Aroon Up line.",
                )),
            ),
            color_input(
                BEARISH,
                msg("indicator.aroon.downColor", "Aroon Down"),
                "downColor",
                Some(msg(
                    "indicator.aroon.downColor.tooltip",
                    "Color of the Aroon Down line.",
                )),
            ),
        ],
        |bars, inputs| {
            let length = len(inputs, "length", 14);
            let n = length as f64;
            let up = map(&bars_since_highest(&highs(bars), length), |x| {
                (100.0 * (n - x)) / n
            });
            let down = map(&bars_since_lowest(&lows(bars), length), |x| {
                (100.0 * (n - x)) / n
            });
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new(
                        "up",
                        msg("indicator.aroon.plot.up", "Aroon Up"),
                        up,
                        text(inputs, "upColor", BULLISH),
                    ),
                    ClassicPlot::new(
                        "down",
                        msg("indicator.aroon.plot.down", "Aroon Down"),
                        down,
                        text(inputs, "downColor", BEARISH),
                    ),
                ],
                ..Default::default()
            }
        },
    )
}

fn adx() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "average-directional-index",
        msg("indicator.averageDirectionalIndex.title", "Average Directional Index"),
        false,
        vec![
            length_input(
                14.0,
                "diLength",
                msg("indicator.averageDirectionalIndex.diLength", "DI Length"),
                MAX_LENGTH,
                Some(msg("indicator.averageDirectionalIndex.diLength.tooltip", "Wilder RMA smoothing length applied to directional movement and true range when computing +DI and −DI.")),
            ),
            length_input(
                14.0,
                "adxLength",
                msg("indicator.averageDirectionalIndex.adxLength", "ADX Smoothing"),
                MAX_LENGTH,
                Some(msg("indicator.averageDirectionalIndex.adxLength.tooltip", "Wilder RMA smoothing length applied to DX to obtain the ADX line.")),
            ),
            float_input(
                "keyLevel",
                msg("indicator.averageDirectionalIndex.keyLevel", "Key Level"),
                25.0,
                0.0,
                100.0,
                1.0,
                Some(msg("indicator.averageDirectionalIndex.keyLevel.tooltip", "Trend strength reference level; ADX above it is commonly read as a trending market.")),
            ),
            color_input(
                BULLISH,
                msg("indicator.averageDirectionalIndex.plusDiColor", "+DI Color"),
                "plusDiColor",
                Some(msg("indicator.averageDirectionalIndex.plusDiColor.tooltip", "Color of the plus directional indicator, and of the ADX line above the key level while +DI leads.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.averageDirectionalIndex.minusDiColor", "−DI Color"),
                "minusDiColor",
                Some(msg("indicator.averageDirectionalIndex.minusDiColor.tooltip", "Color of the minus directional indicator, and of the ADX line above the key level while −DI leads.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.averageDirectionalIndex.adxColor", "ADX Color"),
                "adxColor",
                Some(msg("indicator.averageDirectionalIndex.adxColor.tooltip", "Color of the ADX line while it sits below the key level — no trend strong enough to trade.")),
            ),
        ],
        |bars, inputs| {
            let di_len = len(inputs, "diLength", 14);
            let n = bars.len();
            let mut plus_dm = vec![f64::NAN; n];
            let mut minus_dm = vec![f64::NAN; n];
            for i in 1..n {
                let up = bars[i].high - bars[i - 1].high;
                let dn = bars[i - 1].low - bars[i].low;
                plus_dm[i] = if up > dn && up > 0.0 { up } else { 0.0 };
                minus_dm[i] = if dn > up && dn > 0.0 { dn } else { 0.0 };
            }
            let atr_line = rma(&true_range(bars), di_len);
            let directional_index = |dm: &[f64]| zip(&rma(dm, di_len), &atr_line, |d, a| if a == 0.0 { 0.0 } else { (100.0 * d) / a });
            let plus_di = directional_index(&plus_dm);
            let minus_di = directional_index(&minus_dm);
            let dx = zip(&plus_di, &minus_di, |p, m| if p + m == 0.0 { 0.0 } else { (100.0 * (p - m).abs()) / (p + m) });
            let adx_line = rma(&dx, len(inputs, "adxLength", 14));
            let plus_ink = text(inputs, "plusDiColor", BULLISH);
            let minus_ink = text(inputs, "minusDiColor", BEARISH);
            let neutral_ink = text(inputs, "adxColor", NEUTRAL);
            let key_level = num(inputs, "keyLevel", 25.0);
            // The ADX borrows the dominant DI's color once it clears the key level.
            let adx_colors: Vec<Option<String>> = adx_line
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    if !x.is_finite() {
                        return None;
                    }
                    if x <= key_level {
                        return Some(neutral_ink.clone());
                    }
                    Some(if plus_di[i] > minus_di[i] { plus_ink.clone() } else { minus_ink.clone() })
                })
                .collect();
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("plusDi", msg("indicator.averageDirectionalIndex.plot.plusDi", "+DI"), plus_di.clone(), transp(&plus_ink, 30.0)),
                    ClassicPlot::new("minusDi", "-DI", minus_di.clone(), transp(&minus_ink, 30.0)).display_title(msg("indicator.averageDirectionalIndex.plot.minusDi", "−DI")),
                    ClassicPlot::new("adx", msg("indicator.averageDirectionalIndex.plot.adx", "ADX"), adx_line, neutral_ink).width(2.0).colors(adx_colors),
                ],
                levels: vec![
                    ClassicLevel::new("keyLevel", key_level)
                        .color(NEUTRAL)
                        .line_style(LineStyle::Dashed)
                        .title(msg("indicator.averageDirectionalIndex.level.keyLevel", "Key Level")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.averageDirectionalIndex.shortTitle", "ADX"))
}

fn vortex() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "vortex-indicator",
        msg("indicator.vortexIndicator.title", "Vortex Indicator"),
        false,
        vec![
            length_input(
                14.0,
                "length",
                msg("indicator.vortexIndicator.length", "Length"),
                MAX_LENGTH,
                Some(msg("indicator.vortexIndicator.length.tooltip", "Number of bars used to sum the vortex movements (VM+ and VM−) and the true range.")),
            ),
            color_input(BULLISH, msg("indicator.vortexIndicator.plusColor", "VI+"), "plusColor", Some(msg("indicator.vortexIndicator.plusColor.tooltip", "Color of the positive vortex line (VI+)."))),
            color_input(BEARISH, msg("indicator.vortexIndicator.minusColor", "VI−"), "minusColor", Some(msg("indicator.vortexIndicator.minusColor.tooltip", "Color of the negative vortex line (VI−)."))),
            color_input(
                NEUTRAL,
                msg("indicator.vortexIndicator.baselineColor", "Baseline"),
                "baselineColor",
                Some(msg("indicator.vortexIndicator.baselineColor.tooltip", "Color of the dotted baseline drawn at 1.")),
            ),
        ],
        |bars, inputs| {
            let length = len(inputs, "length", 14);
            let n = bars.len();
            let mut vm_plus = vec![f64::NAN; n];
            let mut vm_minus = vec![f64::NAN; n];
            for i in 1..n {
                vm_plus[i] = (bars[i].high - bars[i - 1].low).abs();
                vm_minus[i] = (bars[i].low - bars[i - 1].high).abs();
            }
            let tr_sum = sum(&true_range(bars), length);
            let ratio = |vm: &[f64]| zip(&sum(vm, length), &tr_sum, |v, t| if t == 0.0 { f64::NAN } else { v / t });
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("viPlus", msg("indicator.vortexIndicator.plot.viPlus", "VI+"), ratio(&vm_plus), text(inputs, "plusColor", BULLISH)),
                    ClassicPlot::new("viMinus", "VI-", ratio(&vm_minus), text(inputs, "minusColor", BEARISH)).display_title(msg("indicator.vortexIndicator.plot.viMinus", "VI−")),
                ],
                levels: vec![
                    ClassicLevel::new("baseline", 1.0)
                        .color(text(inputs, "baselineColor", NEUTRAL))
                        .line_style(LineStyle::Dotted)
                        .title(msg("indicator.vortexIndicator.level.baseline", "Baseline")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.vortexIndicator.shortTitle", "VI"))
}

/// Every trend-strength study, in the original's order.
pub fn trend_specs() -> Vec<ClassicIndicatorSpec> {
    vec![aroon(), adx(), vortex()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ohlcv;

    fn bars(n: usize) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| {
                let c = 100.0 + 10.0 * (i as f64 / 15.0).sin() + (i % 7) as f64;
                Ohlcv::new(i as i64 * 60_000, c, c + 1.0, c - 1.0, c, Some(1000.0))
            })
            .collect()
    }

    fn spec(kind: &str) -> ClassicIndicatorSpec {
        trend_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    #[test]
    fn ships_the_catalog_in_order() {
        let types: Vec<&str> = trend_specs().iter().map(|s| s.indicator_type).collect();
        assert_eq!(
            types,
            ["aroon", "average-directional-index", "vortex-indicator"]
        );
    }

    #[test]
    fn every_spec_computes_bar_aligned_plots_on_defaults() {
        let data = bars(400);
        for s in trend_specs() {
            assert!(!s.overlay);
            let out = s.compute(&data, &s.default_inputs());
            assert!(!out.plots.is_empty());
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    data.len(),
                    "{}:{}",
                    s.indicator_type,
                    plot.key
                );
                assert!(
                    plot.values.iter().any(|v| v.is_finite()),
                    "{}:{}",
                    s.indicator_type,
                    plot.key
                );
            }
        }
    }

    #[test]
    fn aroon_reads_100_on_a_fresh_high_and_stays_in_range() {
        // Strictly rising highs: the newest bar is always the window's highest.
        let rising: Vec<Ohlcv> = (0..40)
            .map(|i| {
                Ohlcv::new(
                    i as i64,
                    10.0 + i as f64,
                    11.0 + i as f64,
                    9.0 + i as f64,
                    10.0 + i as f64,
                    None,
                )
            })
            .collect();
        let s = spec("aroon");
        let out = s.compute(&rising, &s.default_inputs());
        let (up, down) = (&out.plots[0].values, &out.plots[1].values);
        assert!(up[12].is_nan());
        assert_eq!(up[13], 100.0);
        assert_eq!(up[39], 100.0);
        // The window's lowest low is its oldest bar, 13 bars back.
        assert!((down[39] - (100.0 * (14.0 - 13.0)) / 14.0).abs() < 1e-12);

        let data = bars(400);
        let wavy = s.compute(&data, &s.default_inputs());
        assert!(
            wavy.plots
                .iter()
                .flat_map(|p| &p.values)
                .filter(|v| v.is_finite())
                .all(|v| (0.0..=100.0).contains(v))
        );
    }

    #[test]
    fn adx_names_its_minus_di_plot_with_ascii_identity_and_a_true_minus_label() {
        let data = bars(400);
        let s = spec("average-directional-index");
        let out = s.compute(&data, &s.default_inputs());
        let keys: Vec<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["plusDi", "minusDi", "adx"]);
        assert_eq!(out.plots[1].title, "-DI".into());
        assert_eq!(
            out.plots[1]
                .display_title
                .as_ref()
                .map(|m| m.source_text())
                .as_deref(),
            Some("−DI")
        );
        assert_eq!(
            out.levels.iter().map(|l| l.price).collect::<Vec<_>>(),
            [25.0]
        );
        assert!(
            out.plots[2]
                .values
                .iter()
                .filter(|v| v.is_finite())
                .all(|v| (0.0..=100.0).contains(v))
        );
    }

    #[test]
    fn adx_warms_up_over_both_smoothings() {
        let data = bars(100);
        let s = spec("average-directional-index");
        let out = s.compute(&data, &s.default_inputs());
        // +DI/-DI need 14 directional moves (bars 1..=14), the ADX another 14 DX values.
        assert!(out.plots[0].values[13].is_nan());
        assert!(out.plots[0].values[14].is_finite());
        assert!(out.plots[2].values[26].is_nan());
        assert!(out.plots[2].values[27].is_finite());
    }

    #[test]
    fn vortex_matches_a_hand_computed_ratio_and_keeps_its_baseline() {
        let data = bars(100);
        let s = spec("vortex-indicator");
        let out = s.compute(&data, &s.default_inputs());
        assert_eq!(out.plots[1].title, "VI-".into());
        assert_eq!(
            out.plots[1]
                .display_title
                .as_ref()
                .map(|m| m.source_text())
                .as_deref(),
            Some("VI−")
        );
        assert_eq!(out.levels.len(), 1);
        assert_eq!(out.levels[0].price, 1.0);
        assert_eq!(out.levels[0].line_style, Some(LineStyle::Dotted));
        // First usable value needs 14 vortex movements (bars 1..=14).
        assert!(out.plots[0].values[13].is_nan());
        let hand: f64 = (1..=14)
            .map(|i| (data[i].high - data[i - 1].low).abs())
            .sum::<f64>()
            / sum(&true_range(&data), 14)[14];
        assert!((out.plots[0].values[14] - hand).abs() < 1e-12);
    }
}
