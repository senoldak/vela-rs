//! Bands and channels: envelopes around price plus their derived oscillators.

use crate::i18n::{MessageLike, msg};
use crate::model::{FillGradientStop, Inputs, LineLikeKind, LineStyle, Ohlcv, SeriesDisplay};
use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL, WARNING};

use super::define::{
    ClassicBand, ClassicIndicatorSpec, ClassicInput, ClassicLevel, ClassicOutput, ClassicPlot,
    flag, len, num, text,
};
use super::math::{
    atr, ema, highest, highs, js_max, js_min, lowest, lows, map, rma, shift, sma, source_values,
    stdev, sub, zip,
};
use super::shared::{
    BASE_TRANSPARENCY, DirectionInk, STYLE, bool_input, channel_style_inputs, color_input,
    directional_channel, float_input, int_input, length_input, option_input, source_input, transp,
};

/// Bill Williams' balance lines are drawn in their own inks, unrelated to direction.
const JAW_COLOR: &str = "#ab47bc";
const TEETH_COLOR: &str = WARNING;
const LIPS_COLOR: &str = "#ffd700";

struct Bollinger {
    src: Vec<f64>,
    basis: Vec<f64>,
    upper: Vec<f64>,
    lower: Vec<f64>,
}

fn bollinger(bars: &[Ohlcv], inputs: &Inputs) -> Bollinger {
    let window = len(inputs, "length", 20);
    let mult = num(inputs, "mult", 2.0);
    let src = source_values(bars, &text(inputs, "source", "Close"));
    let basis = sma(&src, window);
    let dev = map(&stdev(&src, window), |x| x * mult);
    let upper = zip(&basis, &dev, |b, d| b + d);
    let lower = zip(&basis, &dev, |b, d| b - d);
    Bollinger {
        src,
        basis,
        upper,
        lower,
    }
}

/// Length / source / multiplier, as the derived %B and BandWidth studies declare them.
fn bollinger_inputs(mult_min: f64, mult_step: f64) -> Vec<ClassicInput> {
    let mut length = length_input(
        20.0,
        "length",
        msg("indicator.common.length", "Length"),
        5000.0,
        Some(msg(
            "indicator.bollinger.length.tooltip",
            "Number of bars used for the Bollinger Bands basis (a simple moving average) and for the standard deviation.",
        )),
    );
    length.min = Some(2.0);
    vec![
        length,
        source_input(
            "Close",
            "source",
            msg("indicator.common.source", "Source"),
            Some(msg(
                "indicator.bollinger.source.tooltip",
                "Price series the bands are computed from.",
            )),
        ),
        float_input(
            "mult",
            msg("indicator.bollinger.mult", "Multiplier"),
            2.0,
            mult_min,
            50.0,
            mult_step,
            Some(msg(
                "indicator.bollinger.mult.tooltip",
                "Standard deviation multiplier for the upper and lower bands.",
            )),
        ),
    ]
}

fn bollinger_bands() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "bollinger-bands",
        msg("indicator.bollingerBands.title", "Bollinger Bands"),
        true,
        vec![
            length_input(
                20.0,
                "length",
                msg("indicator.bollingerBands.length", "Length"),
                5000.0,
                Some(msg("indicator.bollingerBands.length.tooltip", "Number of bars used to calculate the basis SMA and the standard deviation.")),
            ),
            source_input(
                "Close",
                "source",
                msg("indicator.bollingerBands.source", "Source"),
                Some(msg("indicator.bollingerBands.source.tooltip", "Price series the bands are calculated on.")),
            ),
            float_input(
                "mult",
                msg("indicator.bollingerBands.mult", "Multiplier"),
                2.0,
                0.0,
                50.0,
                0.1,
                Some(msg("indicator.bollingerBands.mult.tooltip", "Number of standard deviations added above and below the basis to form the bands.")),
            ),
            color_input(
                WARNING,
                msg("indicator.bollingerBands.basisColor", "Basis"),
                "basisColor",
                Some(msg("indicator.bollingerBands.basisColor.tooltip", "Color of the basis (middle band) moving average.")),
            ),
            color_input(
                INFO,
                msg("indicator.bollingerBands.color", "Bands"),
                "color",
                Some(msg("indicator.bollingerBands.color.tooltip", "Color of the upper and lower bands.")),
            ),
            color_input(
                &transp(INFO, 92.0),
                msg("indicator.bollingerBands.fillColor", "Fill"),
                "fillColor",
                Some(msg("indicator.bollingerBands.fillColor.tooltip", "Fill color of the area between the upper and lower bands.")),
            ),
        ],
        |bars, inputs| {
            let Bollinger { basis, upper, lower, .. } = bollinger(bars, inputs);
            let ink = text(inputs, "color", INFO);
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("basis", msg("indicator.bollingerBands.plot.basis", "Basis"), basis, text(inputs, "basisColor", WARNING)),
                    ClassicPlot::new("upper", msg("indicator.bollingerBands.plot.upper", "Upper"), upper, ink.clone()),
                    ClassicPlot::new("lower", msg("indicator.bollingerBands.plot.lower", "Lower"), lower, ink),
                ],
                bands: vec![ClassicBand::new("bb", "upper", "lower", text(inputs, "fillColor", &transp(INFO, 92.0)))],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.bollingerBands.shortTitle", "BB"))
}

const PERCENT_B_MID: f64 = 0.5;
const THRESHOLDS: &str = "Thresholds";

fn percent_b() -> ClassicIndicatorSpec {
    let mut inputs = bollinger_inputs(0.001, 0.25);
    inputs.extend([
        float_input(
            "overbought",
            msg("indicator.percentB.overbought", "Overbought"),
            1.0,
            -10.0,
            10.0,
            0.05,
            Some(msg(
                "indicator.percentB.overbought.tooltip",
                "Level treated as overbought. 1 is the upper band itself, so readings above the default mean price sits outside the bands.",
            )),
        )
        .group(THRESHOLDS),
        float_input(
            "oversold",
            msg("indicator.percentB.oversold", "Oversold"),
            0.0,
            -10.0,
            10.0,
            0.05,
            Some(msg(
                "indicator.percentB.oversold.tooltip",
                "Level treated as oversold. 0 is the lower band itself, so readings below the default mean price sits outside the bands.",
            )),
        )
        .group(THRESHOLDS),
        color_input(
            BULLISH,
            msg("indicator.percentB.bullColor", "Bullish"),
            "bullColor",
            Some(msg("indicator.percentB.bullColor.tooltip", "Color of %B above the 0.5 midline, where price trades in the upper half of the bands.")),
        ),
        color_input(
            BEARISH,
            msg("indicator.percentB.bearColor", "Bearish"),
            "bearColor",
            Some(msg("indicator.percentB.bearColor.tooltip", "Color of %B below the 0.5 midline, where price trades in the lower half of the bands.")),
        ),
        bool_input(
            "gradientFill",
            msg("indicator.percentB.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            Some(msg(
                "indicator.percentB.gradientFill.tooltip",
                "Vertical gradient between %B and the 0.5 midline, fully transparent at the midline and more opaque at the oscillator value.",
            )),
        ),
        color_input(
            NEUTRAL,
            msg("indicator.percentB.levelsColor", "Levels Color"),
            "levelsColor",
            Some(msg("indicator.percentB.levelsColor.tooltip", "Color of the dashed overbought/oversold levels and of the dotted midline.")),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "percent-b",
        msg("indicator.percentB.title", "Bollinger Bands %B"),
        false,
        inputs,
        |bars, inputs| {
            let Bollinger {
                src, upper, lower, ..
            } = bollinger(bars, inputs);
            let values: Vec<f64> = src
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    let (u, l) = (upper[i], lower[i]);
                    if u.is_finite() && l.is_finite() && u != l {
                        (x - l) / (u - l)
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let levels_ink = text(inputs, "levelsColor", NEUTRAL);
            let colors: Vec<Option<String>> = values
                .iter()
                .map(|&x| {
                    x.is_finite().then(|| {
                        if x > PERCENT_B_MID {
                            bull.clone()
                        } else {
                            bear.clone()
                        }
                    })
                })
                .collect();
            let mid_values: Vec<f64> = values
                .iter()
                .map(|&x| {
                    if x.is_finite() {
                        PERCENT_B_MID
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            let plots = vec![
                ClassicPlot::new(
                    "pb",
                    msg("indicator.percentB.plot.pb", "%B"),
                    values.clone(),
                    bear.clone(),
                )
                .colors(colors),
                ClassicPlot::new(
                    "mid",
                    msg("indicator.percentB.plot.mid", "Midline"),
                    mid_values,
                    levels_ink.clone(),
                )
                .display(SeriesDisplay::hidden()),
            ];
            let mut bands = Vec::new();
            if flag(inputs, "gradientFill", true) {
                // Opaque at the oscillator, invisible where it meets the midline.
                let gradient: Vec<Option<FillGradientStop>> = values
                    .iter()
                    .map(|&x| {
                        if !x.is_finite() {
                            return None;
                        }
                        let up = x > PERCENT_B_MID;
                        let ink = if up { &bull } else { &bear };
                        Some(FillGradientStop {
                            top_value: x.max(PERCENT_B_MID),
                            bottom_value: x.min(PERCENT_B_MID),
                            top_color: if up {
                                transp(ink, 50.0)
                            } else {
                                transp(ink, 100.0)
                            },
                            bottom_color: if up {
                                transp(ink, 100.0)
                            } else {
                                transp(ink, 50.0)
                            },
                        })
                    })
                    .collect();
                bands.push(
                    ClassicBand::new("pbFill", "pb", "mid", transp(NEUTRAL, 100.0))
                        .gradient(gradient),
                );
            }
            ClassicOutput {
                plots,
                bands,
                levels: vec![
                    ClassicLevel::new("overbought", num(inputs, "overbought", 1.0))
                        .color(levels_ink.clone())
                        .line_style(LineStyle::Dashed)
                        .title(msg("indicator.percentB.level.overbought", "Overbought")),
                    ClassicLevel::new("oversold", num(inputs, "oversold", 0.0))
                        .color(levels_ink.clone())
                        .line_style(LineStyle::Dashed)
                        .title(msg("indicator.percentB.level.oversold", "Oversold")),
                    ClassicLevel::new("midline", PERCENT_B_MID)
                        .color(transp(&levels_ink, 50.0))
                        .line_style(LineStyle::Dotted)
                        .title(msg("indicator.percentB.level.midline", "Midline")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.percentB.shortTitle", "%B"))
}

const SQUEEZE_BULGE: &str = "Squeeze & Bulge";

fn bandwidth() -> ClassicIndicatorSpec {
    let mut inputs = bollinger_inputs(0.001, 0.25);
    inputs.extend([
        int_input(
            "lookback",
            msg("indicator.bandwidth.lookback", "Reference Lookback"),
            125.0,
            2.0,
            5000.0,
            Some(msg("indicator.bandwidth.lookback.tooltip", "Window used to locate the lowest (Squeeze) and highest (Bulge) BandWidth references.")),
        )
        .group(SQUEEZE_BULGE),
        bool_input(
            "showSqueeze",
            msg("indicator.bandwidth.showSqueeze", "Show Squeeze Level"),
            true,
            SQUEEZE_BULGE,
            Some(msg(
                "indicator.bandwidth.showSqueeze.tooltip",
                "Plots the lowest BandWidth of the reference lookback \u{2014} unusually compressed volatility, a condition rather than a directional signal.",
            )),
        ),
        bool_input(
            "showBulge",
            msg("indicator.bandwidth.showBulge", "Show Bulge Level"),
            true,
            SQUEEZE_BULGE,
            Some(msg("indicator.bandwidth.showBulge.tooltip", "Plots the highest BandWidth of the reference lookback \u{2014} extreme width that is unlikely to be sustained.")),
        ),
        color_input(
            NEUTRAL,
            msg("indicator.bandwidth.color", "BandWidth"),
            "color",
            Some(msg(
                "indicator.bandwidth.color.tooltip",
                "Color of the BandWidth line. At its Squeeze reference the line takes the Squeeze color, at its Bulge reference the Bulge color.",
            )),
        ),
        bool_input(
            "gradientFill",
            msg("indicator.bandwidth.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            Some(msg("indicator.bandwidth.gradientFill.tooltip", "Fills the area between zero and BandWidth with a vertical gradient, fully transparent at zero.")),
        ),
        color_input(
            WARNING,
            msg("indicator.bandwidth.squeezeColor", "Squeeze Level"),
            "squeezeColor",
            Some(msg("indicator.bandwidth.squeezeColor.tooltip", "Color of the lowest-BandWidth reference line.")),
        ),
        color_input(
            BEARISH,
            msg("indicator.bandwidth.bulgeColor", "Bulge Level"),
            "bulgeColor",
            Some(msg("indicator.bandwidth.bulgeColor.tooltip", "Color of the highest-BandWidth reference line.")),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "bandwidth",
        msg("indicator.bandwidth.title", "Bollinger Bands Width"),
        false,
        inputs,
        |bars, inputs| {
            let Bollinger {
                basis,
                upper,
                lower,
                ..
            } = bollinger(bars, inputs);
            // Dividing the span by the basis normalizes it, so readings compare across instruments.
            let values: Vec<f64> = basis
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    let (u, l) = (upper[i], lower[i]);
                    if b.is_finite() && b != 0.0 && u.is_finite() && l.is_finite() {
                        (u - l) / b
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            let lookback = len(inputs, "lookback", 125);
            let squeeze_level = lowest(&values, lookback);
            let bulge_level = highest(&values, lookback);
            let base = text(inputs, "color", NEUTRAL);
            let squeeze_ink = text(inputs, "squeezeColor", WARNING);
            let bulge_ink = text(inputs, "bulgeColor", BEARISH);
            let colors: Vec<Option<String>> = values
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    if !x.is_finite() {
                        None
                    } else if x <= squeeze_level[i] {
                        Some(squeeze_ink.clone())
                    } else if x >= bulge_level[i] {
                        Some(bulge_ink.clone())
                    } else {
                        Some(base.clone())
                    }
                })
                .collect();
            let zero_values: Vec<f64> = values
                .iter()
                .map(|&x| if x.is_finite() { 0.0 } else { f64::NAN })
                .collect();
            let mut plots = vec![
                ClassicPlot::new(
                    "bbw",
                    msg("indicator.bandwidth.plot.bbw", "BandWidth"),
                    values.clone(),
                    base.clone(),
                )
                .colors(colors.clone()),
                ClassicPlot::new(
                    "zero",
                    msg("indicator.bandwidth.plot.zero", "Zero Line"),
                    zero_values,
                    transp(NEUTRAL, 90.0),
                ),
            ];
            if flag(inputs, "showSqueeze", true) {
                plots.push(ClassicPlot::new(
                    "squeeze",
                    msg("indicator.bandwidth.plot.squeeze", "Squeeze Level"),
                    squeeze_level,
                    squeeze_ink,
                ));
            }
            if flag(inputs, "showBulge", true) {
                plots.push(ClassicPlot::new(
                    "bulge",
                    msg("indicator.bandwidth.plot.bulge", "Bulge Level"),
                    bulge_level,
                    bulge_ink,
                ));
            }
            let mut bands = Vec::new();
            if flag(inputs, "gradientFill", true) {
                let gradient: Vec<Option<FillGradientStop>> = values
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        if !x.is_finite() {
                            return None;
                        }
                        let ink = colors[i].as_deref().unwrap_or(&base);
                        Some(FillGradientStop {
                            top_value: x,
                            bottom_value: 0.0,
                            top_color: transp(ink, 50.0),
                            bottom_color: transp(ink, BASE_TRANSPARENCY),
                        })
                    })
                    .collect();
                bands.push(
                    ClassicBand::new("bbwFill", "bbw", "zero", transp(NEUTRAL, 100.0))
                        .gradient(gradient),
                );
            }
            ClassicOutput {
                plots,
                bands,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.bandwidth.shortTitle", "BandWidth"))
}

fn keltner() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        length_input(
            20.0,
            "length",
            msg("indicator.keltnerChannels.length", "EMA Length"),
            5000.0,
            Some(msg(
                "indicator.keltnerChannels.length.tooltip",
                "Number of bars used to compute the exponential moving average basis line.",
            )),
        ),
        int_input(
            "atrLength",
            msg("indicator.keltnerChannels.atrLength", "ATR Length"),
            10.0,
            1.0,
            5000.0,
            Some(msg(
                "indicator.keltnerChannels.atrLength.tooltip",
                "Number of bars used to compute the Average True Range (Wilder smoothing).",
            )),
        ),
        float_input(
            "mult",
            msg("indicator.keltnerChannels.mult", "Multiplier"),
            2.0,
            0.0,
            50.0,
            0.1,
            Some(msg(
                "indicator.keltnerChannels.mult.tooltip",
                "ATR multiple used to offset the upper and lower bands from the basis line.",
            )),
        ),
        source_input(
            "Close",
            "source",
            msg("indicator.keltnerChannels.source", "Source"),
            Some(msg(
                "indicator.keltnerChannels.source.tooltip",
                "Price series used to compute the basis line.",
            )),
        ),
    ];
    inputs.extend(channel_style_inputs(Some("Basis Color".into())));
    ClassicIndicatorSpec::new(
        "keltner-channels",
        msg("indicator.keltnerChannels.title", "Keltner Channels"),
        true,
        inputs,
        |bars, inputs| {
            let basis = ema(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "length", 20),
            );
            let mult = num(inputs, "mult", 2.0);
            let range = map(&atr(bars, len(inputs, "atrLength", 10)), |x| x * mult);
            let ink = DirectionInk {
                bull: text(inputs, "bullColor", BULLISH),
                bear: text(inputs, "bearColor", BEARISH),
                neutral: text(inputs, "basisColor", NEUTRAL),
            };
            let upper = zip(&basis, &range, |b, r| b + r);
            let lower = zip(&basis, &range, |b, r| b - r);
            let parts = directional_channel(bars, &basis, &upper, &lower, &ink, "kc");
            ClassicOutput {
                plots: parts.plots,
                bands: parts.bands,
                levels: parts.levels,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.keltnerChannels.shortTitle", "KC"))
}

fn donchian() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "donchian-channels",
        msg("indicator.donchianChannels.title", "Donchian Channels"),
        true,
        vec![
            length_input(
                20.0,
                "length",
                msg("indicator.donchianChannels.length", "Length"),
                5000.0,
                Some(msg("indicator.donchianChannels.length.tooltip", "Lookback length. The upper and lower channels are the highest high and lowest low over this many bars.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.donchianChannels.upperColor", "Upper"),
                "upperColor",
                Some(msg("indicator.donchianChannels.upperColor.tooltip", "Color of the upper channel line.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.donchianChannels.middleColor", "Middle"),
                "middleColor",
                Some(msg("indicator.donchianChannels.middleColor.tooltip", "Color of the channel midline.")),
            ),
            color_input(
                BULLISH,
                msg("indicator.donchianChannels.lowerColor", "Lower"),
                "lowerColor",
                Some(msg("indicator.donchianChannels.lowerColor.tooltip", "Color of the lower channel line.")),
            ),
            color_input(
                &transp(NEUTRAL, 92.0),
                msg("indicator.donchianChannels.fillColor", "Fill"),
                "fillColor",
                Some(msg("indicator.donchianChannels.fillColor.tooltip", "Color of the channel interior fill.")),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 20);
            let upper = highest(&highs(bars), window);
            let lower = lowest(&lows(bars), window);
            let middle = zip(&upper, &lower, |u, l| (u + l) / 2.0);
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("upper", msg("indicator.donchianChannels.plot.upper", "Upper"), upper, text(inputs, "upperColor", BEARISH)),
                    ClassicPlot::new("middle", msg("indicator.donchianChannels.plot.middle", "Middle"), middle, text(inputs, "middleColor", NEUTRAL)).kind(LineLikeKind::Circles),
                    ClassicPlot::new("lower", msg("indicator.donchianChannels.plot.lower", "Lower"), lower, text(inputs, "lowerColor", BULLISH)),
                ],
                bands: vec![ClassicBand::new("dc", "upper", "lower", text(inputs, "fillColor", &transp(NEUTRAL, 92.0)))],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.donchianChannels.shortTitle", "DC"))
}

fn chandelier() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "chandelier-exit",
        msg("indicator.chandelierExit.title", "Chandelier Exit"),
        true,
        vec![
            length_input(
                22.0,
                "length",
                msg("indicator.chandelierExit.length", "Length"),
                5000.0,
                Some(msg("indicator.chandelierExit.length.tooltip", "Lookback used for both the highest high / lowest low extremes and the ATR.")),
            ),
            float_input(
                "mult",
                msg("indicator.chandelierExit.mult", "ATR Multiplier"),
                3.0,
                0.0,
                50.0,
                0.1,
                Some(msg("indicator.chandelierExit.mult.tooltip", "Multiple of ATR subtracted from the highest high (long stop) and added to the lowest low (short stop).")),
            ),
            bool_input(
                "ratchet",
                msg("indicator.chandelierExit.ratchet", "Ratchet Stops"),
                false,
                super::shared::SETTINGS,
                Some(msg(
                    "indicator.chandelierExit.ratchet.tooltip",
                    "Prevents the long stop from falling and the short stop from rising, resetting once close crosses through the stop.",
                )),
            ),
            color_input(
                BULLISH,
                msg("indicator.chandelierExit.longColor", "Long Stop"),
                "longColor",
                Some(msg("indicator.chandelierExit.longColor.tooltip", "Color of the long trailing stop line.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.chandelierExit.shortColor", "Short Stop"),
                "shortColor",
                Some(msg("indicator.chandelierExit.shortColor.tooltip", "Color of the short trailing stop line.")),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 22);
            let mult = num(inputs, "mult", 3.0);
            let range = map(&atr(bars, window), |x| x * mult);
            let basic_long = zip(&highest(&highs(bars), window), &range, |h, r| h - r);
            let basic_short = zip(&lowest(&lows(bars), window), &range, |l, r| l + r);
            let (mut long, mut short) = (basic_long.clone(), basic_short.clone());
            if flag(inputs, "ratchet", false) {
                let n = bars.len();
                long = vec![f64::NAN; n];
                short = vec![f64::NAN; n];
                for i in 0..n {
                    let (bl, bs) = (basic_long[i], basic_short[i]);
                    let prev_close = if i > 0 { bars[i - 1].close } else { bars[i].close };
                    let prev_long = if i > 0 && long[i - 1].is_finite() { long[i - 1] } else { bl };
                    let prev_short = if i > 0 && short[i - 1].is_finite() { short[i - 1] } else { bs };
                    long[i] = if bl.is_finite() { if prev_close > prev_long { js_max(bl, prev_long) } else { bl } } else { f64::NAN };
                    short[i] = if bs.is_finite() { if prev_close < prev_short { js_min(bs, prev_short) } else { bs } } else { f64::NAN };
                }
            }
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("long", msg("indicator.chandelierExit.plot.long", "Long Stop"), long, text(inputs, "longColor", BULLISH)),
                    ClassicPlot::new("short", msg("indicator.chandelierExit.plot.short", "Short Stop"), short, text(inputs, "shortColor", BEARISH)),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.chandelierExit.shortTitle", "CE"))
}

fn chande_kroll() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "chande-kroll-stop",
        msg("indicator.chandeKrollStop.title", "Chande Kroll Stop"),
        true,
        vec![
            int_input(
                "p",
                msg("indicator.chandeKrollStop.p", "ATR Length (P)"),
                10.0,
                1.0,
                5000.0,
                Some(msg("indicator.chandeKrollStop.p.tooltip", "Lookback used for both the ATR and the highest high / lowest low price extremes.")),
            ),
            float_input(
                "x",
                msg("indicator.chandeKrollStop.x", "ATR Multiplier (X)"),
                1.0,
                0.0,
                50.0,
                0.1,
                Some(msg("indicator.chandeKrollStop.x.tooltip", "Multiplier applied to the ATR to offset the preliminary stops from the price extremes.")),
            ),
            int_input(
                "q",
                msg("indicator.chandeKrollStop.q", "Stop Length (Q)"),
                9.0,
                1.0,
                5000.0,
                Some(msg("indicator.chandeKrollStop.q.tooltip", "Lookback over which the preliminary stops are extended into the final stop levels.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.chandeKrollStop.shortColor", "Short Stop"),
                "shortColor",
                Some(msg("indicator.chandeKrollStop.shortColor.tooltip", "Color of the protective stop line for short positions.")),
            ),
            color_input(
                BULLISH,
                msg("indicator.chandeKrollStop.longColor", "Long Stop"),
                "longColor",
                Some(msg("indicator.chandeKrollStop.longColor.tooltip", "Color of the protective stop line for long positions.")),
            ),
        ],
        |bars, inputs| {
            let p = len(inputs, "p", 10);
            let q = len(inputs, "q", 9);
            let x = num(inputs, "x", 1.0);
            let range = map(&atr(bars, p), |v| v * x);
            let first_high = zip(&highest(&highs(bars), p), &range, |h, r| h - r);
            let first_low = zip(&lowest(&lows(bars), p), &range, |l, r| l + r);
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("stopShort", "Stop Short", highest(&first_high, q), text(inputs, "shortColor", BEARISH))
                        .display_title(msg("indicator.chandeKrollStop.plot.stopShort", "Short Stop")),
                    ClassicPlot::new("stopLong", "Stop Long", lowest(&first_low, q), text(inputs, "longColor", BULLISH))
                        .display_title(msg("indicator.chandeKrollStop.plot.stopLong", "Long Stop")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.chandeKrollStop.shortTitle", "CKS"))
}

fn supertrend() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "supertrend",
        msg("indicator.supertrend.title", "SuperTrend"),
        true,
        vec![
            int_input(
                "atrLength",
                msg("indicator.supertrend.atrLength", "ATR Length"),
                10.0,
                1.0,
                5000.0,
                Some(msg(
                    "indicator.supertrend.atrLength.tooltip",
                    "Number of bars used to compute the Wilder ATR.",
                )),
            ),
            float_input(
                "mult",
                msg("indicator.supertrend.mult", "Factor"),
                3.0,
                0.0,
                50.0,
                0.1,
                Some(msg(
                    "indicator.supertrend.mult.tooltip",
                    "ATR multiplier setting how far the trailing stop sits from the bar midpoint.",
                )),
            ),
            color_input(
                BULLISH,
                msg("indicator.supertrend.upColor", "Uptrend Color"),
                "upColor",
                Some(msg(
                    "indicator.supertrend.upColor.tooltip",
                    "Line color while the trend is up.",
                )),
            ),
            color_input(
                BEARISH,
                msg("indicator.supertrend.downColor", "Downtrend Color"),
                "downColor",
                Some(msg(
                    "indicator.supertrend.downColor.tooltip",
                    "Line color while the trend is down.",
                )),
            ),
        ],
        |bars, inputs| {
            let mult = num(inputs, "mult", 3.0);
            let range = atr(bars, len(inputs, "atrLength", 10));
            let n = bars.len();
            let mut up = vec![f64::NAN; n];
            let mut down = vec![f64::NAN; n];
            let mut final_upper = f64::NAN;
            let mut final_lower = f64::NAN;
            let mut trend = -1;
            for i in 0..n {
                let r = range[i];
                if !r.is_finite() {
                    continue;
                }
                let b = &bars[i];
                let mid = (b.high + b.low) / 2.0;
                let basic_upper = mid + mult * r;
                let basic_lower = mid - mult * r;
                let prev_upper = if final_upper.is_finite() {
                    final_upper
                } else {
                    basic_upper
                };
                let prev_lower = if final_lower.is_finite() {
                    final_lower
                } else {
                    basic_lower
                };
                let prev_close = if i > 0 { bars[i - 1].close } else { b.close };
                // Bands only ratchet while the prior close holds inside them; otherwise they reset.
                final_upper = if prev_close <= prev_upper {
                    js_min(basic_upper, prev_upper)
                } else {
                    basic_upper
                };
                final_lower = if prev_close >= prev_lower {
                    js_max(basic_lower, prev_lower)
                } else {
                    basic_lower
                };
                trend = if b.close > final_upper {
                    1
                } else if b.close < final_lower {
                    -1
                } else {
                    trend
                };
                if trend == 1 {
                    up[i] = final_lower;
                } else {
                    down[i] = final_upper;
                }
            }
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new(
                        "up",
                        msg("indicator.supertrend.plot.up", "Uptrend"),
                        up,
                        text(inputs, "upColor", BULLISH),
                    ),
                    ClassicPlot::new(
                        "down",
                        msg("indicator.supertrend.plot.down", "Downtrend"),
                        down,
                        text(inputs, "downColor", BEARISH),
                    ),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.supertrend.shortTitle", "SuperTrend"))
}

/// An offset in bars as `shift` takes it.
fn offset(inputs: &Inputs, key: &str, fallback: usize) -> i64 {
    len(inputs, key, fallback) as i64
}

fn alligator() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "williams-alligator",
        msg("indicator.williamsAlligator.title", "Williams Alligator"),
        true,
        vec![
            int_input(
                "jawLength",
                msg("indicator.williamsAlligator.jawLength", "Jaw Length"),
                13.0,
                1.0,
                5000.0,
                Some(msg("indicator.williamsAlligator.jawLength.tooltip", "Smoothing length of the Jaw SMMA of median price (hl2), 13 in the classic setup.")),
            ),
            int_input(
                "teethLength",
                msg("indicator.williamsAlligator.teethLength", "Teeth Length"),
                8.0,
                1.0,
                5000.0,
                Some(msg("indicator.williamsAlligator.teethLength.tooltip", "Smoothing length of the Teeth SMMA of median price (hl2), 8 in the classic setup.")),
            ),
            int_input(
                "lipsLength",
                msg("indicator.williamsAlligator.lipsLength", "Lips Length"),
                5.0,
                1.0,
                5000.0,
                Some(msg("indicator.williamsAlligator.lipsLength.tooltip", "Smoothing length of the Lips SMMA of median price (hl2), 5 in the classic setup.")),
            ),
            int_input(
                "jawOffset",
                msg("indicator.williamsAlligator.jawOffset", "Jaw Offset"),
                8.0,
                0.0,
                100.0,
                Some(msg("indicator.williamsAlligator.jawOffset.tooltip", "Number of bars the Jaw plot is shifted into the future, 8 in the classic setup.")),
            ),
            int_input(
                "teethOffset",
                msg("indicator.williamsAlligator.teethOffset", "Teeth Offset"),
                5.0,
                0.0,
                100.0,
                Some(msg("indicator.williamsAlligator.teethOffset.tooltip", "Number of bars the Teeth plot is shifted into the future, 5 in the classic setup.")),
            ),
            int_input(
                "lipsOffset",
                msg("indicator.williamsAlligator.lipsOffset", "Lips Offset"),
                3.0,
                0.0,
                100.0,
                Some(msg("indicator.williamsAlligator.lipsOffset.tooltip", "Number of bars the Lips plot is shifted into the future, 3 in the classic setup.")),
            ),
            color_input(
                JAW_COLOR,
                msg("indicator.williamsAlligator.jawColor", "Jaw"),
                "jawColor",
                Some(msg("indicator.williamsAlligator.jawColor.tooltip", "Color of the Jaw line.")),
            ),
            color_input(
                TEETH_COLOR,
                msg("indicator.williamsAlligator.teethColor", "Teeth"),
                "teethColor",
                Some(msg("indicator.williamsAlligator.teethColor.tooltip", "Color of the Teeth line.")),
            ),
            color_input(
                LIPS_COLOR,
                msg("indicator.williamsAlligator.lipsColor", "Lips"),
                "lipsColor",
                Some(msg("indicator.williamsAlligator.lipsColor.tooltip", "Color of the Lips line.")),
            ),
        ],
        |bars, inputs| {
            let hl2 = source_values(bars, "HL2");
            // Offsets plot each balance line within the available bars (no future timestamps to plot onto).
            let jaw = shift(&rma(&hl2, len(inputs, "jawLength", 13)), offset(inputs, "jawOffset", 8));
            let teeth = shift(&rma(&hl2, len(inputs, "teethLength", 8)), offset(inputs, "teethOffset", 5));
            let lips = shift(&rma(&hl2, len(inputs, "lipsLength", 5)), offset(inputs, "lipsOffset", 3));
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("jaw", msg("indicator.williamsAlligator.plot.jaw", "Jaw"), jaw, text(inputs, "jawColor", JAW_COLOR)),
                    ClassicPlot::new("teeth", msg("indicator.williamsAlligator.plot.teeth", "Teeth"), teeth, text(inputs, "teethColor", TEETH_COLOR)),
                    ClassicPlot::new("lips", msg("indicator.williamsAlligator.plot.lips", "Lips"), lips, text(inputs, "lipsColor", LIPS_COLOR)),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.williamsAlligator.shortTitle", "Alligator"))
}

const GATOR_LINES: &str = "Alligator Lines";
const SMMA_TYPE: &str = "SMMA (Wilder)";

/// Column colors for one histogram side, graded against its own previous bar;
/// an exact tie keeps the state.
fn expansion_colors(v: &[f64], grow: &str, shrink: &str) -> Vec<Option<String>> {
    let mut growing = false;
    v.iter()
        .enumerate()
        .map(|(i, &x)| {
            if !x.is_finite() {
                return None;
            }
            let prev = if i > 0 { v[i - 1] } else { f64::NAN };
            if prev.is_finite() {
                growing = if x > prev {
                    true
                } else if x < prev {
                    false
                } else {
                    growing
                };
            }
            Some(if growing {
                grow.to_string()
            } else {
                shrink.to_string()
            })
        })
        .collect()
}

fn gator() -> ClassicIndicatorSpec {
    let gator_int =
        |key: &str, title: MessageLike, defval: f64, min: f64, max: f64, tooltip: MessageLike| {
            int_input(key, title, defval, min, max, Some(tooltip)).group(GATOR_LINES)
        };
    ClassicIndicatorSpec::new(
        "gator-oscillator",
        msg("indicator.gatorOscillator.title", "Gator Oscillator"),
        false,
        vec![
            source_input(
                "HL2",
                "source",
                msg("indicator.gatorOscillator.source", "Source"),
                Some(msg(
                    "indicator.gatorOscillator.source.tooltip",
                    "Price series fed to the three Alligator averages. Bill Williams' original recipe uses the median price (high + low) / 2.",
                )),
            )
            .group(GATOR_LINES),
            option_input(
                "maType",
                msg("indicator.gatorOscillator.maType", "Smoothing"),
                SMMA_TYPE,
                &[SMMA_TYPE, "SMA", "EMA"],
                Some(msg(
                    "indicator.gatorOscillator.maType.tooltip",
                    "Moving average applied to the source for the jaw, teeth and lips. The original uses the smoothed (Wilder-style) moving average.",
                )),
            )
            .group(GATOR_LINES),
            gator_int("jawLength", msg("indicator.gatorOscillator.jawLength", "Jaw Length"), 13.0, 1.0, 500.0, msg("indicator.gatorOscillator.jawLength.tooltip", "Averaging length of the jaw line.")),
            gator_int(
                "jawOffset",
                msg("indicator.gatorOscillator.jawOffset", "Jaw Offset"),
                8.0,
                0.0,
                100.0,
                msg(
                    "indicator.gatorOscillator.jawOffset.tooltip",
                    "Bars the jaw is displaced forward, 8 in the original recipe. The upper histogram measures the absolute spread between the displaced jaw and teeth.",
                ),
            ),
            gator_int("teethLength", msg("indicator.gatorOscillator.teethLength", "Teeth Length"), 8.0, 1.0, 500.0, msg("indicator.gatorOscillator.teethLength.tooltip", "Averaging length of the teeth line.")),
            gator_int(
                "teethOffset",
                msg("indicator.gatorOscillator.teethOffset", "Teeth Offset"),
                5.0,
                0.0,
                100.0,
                msg("indicator.gatorOscillator.teethOffset.tooltip", "Bars the teeth are displaced forward, 5 in the original recipe. The teeth line enters both spreads."),
            ),
            gator_int("lipsLength", msg("indicator.gatorOscillator.lipsLength", "Lips Length"), 5.0, 1.0, 500.0, msg("indicator.gatorOscillator.lipsLength.tooltip", "Averaging length of the lips line.")),
            gator_int(
                "lipsOffset",
                msg("indicator.gatorOscillator.lipsOffset", "Lips Offset"),
                3.0,
                0.0,
                100.0,
                msg(
                    "indicator.gatorOscillator.lipsOffset.tooltip",
                    "Bars the lips are displaced forward, 3 in the original recipe. The lower histogram measures the negative absolute spread between the displaced teeth and lips.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.gatorOscillator.growColor", "Expanding"),
                "growColor",
                Some(msg("indicator.gatorOscillator.growColor.tooltip", "Column color while a spread grew against its own previous bar \u{2014} the averages are spreading apart.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.gatorOscillator.shrinkColor", "Contracting"),
                "shrinkColor",
                Some(msg(
                    "indicator.gatorOscillator.shrinkColor.tooltip",
                    "Column color while a spread shrank \u{2014} the averages are converging. Color encodes expansion, not trade direction.",
                )),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.gatorOscillator.zeroColor", "Zero Line"),
                "zeroColor",
                Some(msg("indicator.gatorOscillator.zeroColor.tooltip", "Color of the zero line separating the two histograms.")),
            ),
        ],
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "HL2"));
            let kind = text(inputs, "maType", SMMA_TYPE);
            let smooth = |window: usize| -> Vec<f64> {
                match kind.as_str() {
                    "SMA" => sma(&src, window),
                    "EMA" => ema(&src, window),
                    _ => rma(&src, window),
                }
            };
            let jaw = shift(&smooth(len(inputs, "jawLength", 13)), offset(inputs, "jawOffset", 8));
            let teeth = shift(&smooth(len(inputs, "teethLength", 8)), offset(inputs, "teethOffset", 5));
            let lips = shift(&smooth(len(inputs, "lipsLength", 5)), offset(inputs, "lipsOffset", 3));
            let upper = map(&sub(&jaw, &teeth), f64::abs);
            let lower = map(&sub(&teeth, &lips), f64::abs);
            let grow = text(inputs, "growColor", BULLISH);
            let shrink = text(inputs, "shrinkColor", BEARISH);
            let upper_colors = expansion_colors(&upper, &grow, &shrink);
            let lower_colors = expansion_colors(&lower, &grow, &shrink);
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("upper", "Upper Histogram (Jaw - Teeth)", upper, grow.clone())
                        .display_title(msg("indicator.gatorOscillator.plot.upper", "Upper Histogram (Jaw \u{2212} Teeth)"))
                        .kind(LineLikeKind::Columns)
                        .colors(upper_colors)
                        .base(0.0),
                    ClassicPlot::new("lower", "Lower Histogram (Teeth - Lips)", map(&lower, |x| -x), shrink)
                        .display_title(msg("indicator.gatorOscillator.plot.lower", "Lower Histogram (Teeth \u{2212} Lips)"))
                        .kind(LineLikeKind::Columns)
                        .colors(lower_colors)
                        .base(0.0),
                ],
                levels: vec![
                    ClassicLevel::new("zero", 0.0)
                        .color(text(inputs, "zeroColor", NEUTRAL))
                        .line_style(LineStyle::Solid)
                        .title(msg("indicator.gatorOscillator.level.zero", "Zero Line")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.gatorOscillator.shortTitle", "Gator Oscillator"))
}

pub fn band_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        bollinger_bands(),
        percent_b(),
        bandwidth(),
        keltner(),
        donchian(),
        chandelier(),
        chande_kroll(),
        supertrend(),
        alligator(),
        gator(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::source_text;
    use crate::indicators::classics::define::classic_native_output;
    use crate::model::{SeriesData, SeriesSpec};

    fn bar(close: f64, i: usize) -> Ohlcv {
        Ohlcv::new(
            i as i64 * 60_000,
            close,
            close + 1.0,
            close - 1.0,
            close,
            Some(1000.0),
        )
    }

    fn wave(n: usize) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| bar(100.0 + (i as f64 / 7.0).sin() * 10.0 + i as f64 * 0.05, i))
            .collect()
    }

    fn spec(kind: &str) -> ClassicIndicatorSpec {
        band_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    fn points(series: &SeriesSpec) -> Vec<Option<f64>> {
        match &series.data {
            SeriesData::LineLike { points, .. } => points.iter().map(|p| p.value).collect(),
            _ => panic!("line-like series expected"),
        }
    }

    #[test]
    fn declares_every_band_in_order() {
        let types: Vec<&str> = band_specs().iter().map(|s| s.indicator_type).collect();
        assert_eq!(
            types,
            [
                "bollinger-bands",
                "percent-b",
                "bandwidth",
                "keltner-channels",
                "donchian-channels",
                "chandelier-exit",
                "chande-kroll-stop",
                "supertrend",
                "williams-alligator",
                "gator-oscillator"
            ]
        );
    }

    #[test]
    fn every_spec_computes_aligned_plots_on_default_inputs() {
        let bars = wave(400);
        for spec in band_specs() {
            let out = spec.compute(&bars, &spec.default_inputs());
            assert!(!out.plots.is_empty(), "{}", spec.indicator_type);
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    bars.len(),
                    "{} / {}",
                    spec.indicator_type,
                    plot.key
                );
            }
            assert!(
                out.plots
                    .iter()
                    .any(|p| p.values.iter().any(|v| v.is_finite())),
                "{} has no finite value",
                spec.indicator_type
            );
        }
    }

    #[test]
    fn short_titles_match_the_catalog() {
        let expected = [
            ("bollinger-bands", "BB"),
            ("percent-b", "%B"),
            ("bandwidth", "BandWidth"),
            ("keltner-channels", "KC"),
            ("donchian-channels", "DC"),
            ("chandelier-exit", "CE"),
            ("chande-kroll-stop", "CKS"),
            ("supertrend", "SuperTrend"),
            ("williams-alligator", "Alligator"),
            ("gator-oscillator", "Gator Oscillator"),
        ];
        for (kind, short) in expected {
            let spec = spec(kind);
            assert_eq!(
                source_text(spec.short_title.as_ref().unwrap_or(&spec.title)),
                short,
                "{kind}"
            );
        }
    }

    #[test]
    fn bollinger_bands_are_symmetric_around_the_basis() {
        let bars: Vec<Ohlcv> = (0..50).map(|i| bar(100.0 + i as f64, i)).collect();
        let spec = spec("bollinger-bands");
        let out = classic_native_output(&spec, &bars, &spec.default_inputs());
        let at = |k: usize| points(&out.series[k])[49].expect("finite");
        let (b, u, l) = (at(0), at(1), at(2));
        assert!((u - b - (b - l)).abs() < 1e-8);
        assert!(u > l);
    }

    #[test]
    fn percent_b_warms_up_with_the_bands_and_orders_its_levels() {
        let bars = wave(120);
        let spec = spec("percent-b");
        let out = spec.compute(&bars, &spec.default_inputs());
        let pb = &out.plots[0].values;
        assert!(pb[..19].iter().all(|v| v.is_nan()));
        assert!(pb[19..].iter().all(|v| v.is_finite()));
        assert_eq!(
            out.levels
                .iter()
                .map(|l| l.key.as_str())
                .collect::<Vec<_>>(),
            ["overbought", "oversold", "midline"]
        );
    }

    #[test]
    fn supertrend_splits_into_complementary_legs() {
        let up = (0..60).map(|i| bar(100.0 + i as f64 * 2.0, i));
        let down = (0..60).map(|i| bar(220.0 - i as f64 * 2.0, i + 60));
        let bars: Vec<Ohlcv> = up.chain(down).collect();
        let spec = spec("supertrend");
        let out = classic_native_output(&spec, &bars, &spec.default_inputs());
        let titles: Vec<&str> = out.series.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Uptrend", "Downtrend"]);
        let (uptrend, downtrend) = (points(&out.series[0]), points(&out.series[1]));
        assert!(uptrend.iter().any(Option::is_some));
        assert!(downtrend.iter().any(Option::is_some));
        assert!(
            uptrend
                .iter()
                .zip(&downtrend)
                .all(|(u, d)| u.is_none() || d.is_none())
        );
    }

    #[test]
    fn plots_whose_name_reads_better_on_screen_keep_their_identity() {
        let bars = wave(80);
        let named = |kind: &str| -> Vec<(String, Option<String>)> {
            let spec = spec(kind);
            classic_native_output(&spec, &bars, &spec.default_inputs())
                .series
                .iter()
                .map(|s| (s.title.clone(), s.display_title.as_ref().map(source_text)))
                .collect()
        };
        assert_eq!(
            named("chande-kroll-stop"),
            [
                ("Stop Short".to_string(), Some("Short Stop".to_string())),
                ("Stop Long".to_string(), Some("Long Stop".to_string()))
            ]
        );
        assert_eq!(
            named("gator-oscillator"),
            [
                (
                    "Upper Histogram (Jaw - Teeth)".to_string(),
                    Some("Upper Histogram (Jaw \u{2212} Teeth)".to_string())
                ),
                (
                    "Lower Histogram (Teeth - Lips)".to_string(),
                    Some("Lower Histogram (Teeth \u{2212} Lips)".to_string())
                ),
            ]
        );
    }

    #[test]
    fn chandelier_ratchet_never_lets_the_long_stop_fall_while_close_holds_above() {
        let bars: Vec<Ohlcv> = (0..80).map(|i| bar(100.0 + i as f64, i)).collect();
        let spec = spec("chandelier-exit");
        let mut inputs = spec.default_inputs();
        inputs.insert("ratchet".into(), true.into());
        let long = &spec.compute(&bars, &inputs).plots[0].values;
        let finite: Vec<f64> = long.iter().copied().filter(|v| v.is_finite()).collect();
        assert!(!finite.is_empty());
        assert!(finite.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn donchian_middle_is_the_channel_midpoint() {
        let bars = wave(60);
        let spec = spec("donchian-channels");
        let out = spec.compute(&bars, &spec.default_inputs());
        let (u, m, l) = (
            out.plots[0].values[40],
            out.plots[1].values[40],
            out.plots[2].values[40],
        );
        assert!((m - (u + l) / 2.0).abs() < 1e-12);
        assert_eq!(out.plots[1].kind, LineLikeKind::Circles);
    }
}
