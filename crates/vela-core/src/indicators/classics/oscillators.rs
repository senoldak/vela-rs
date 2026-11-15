//! Momentum oscillators: study-pane classics driven by a price source.

use crate::i18n::{MessageLike, msg};
use crate::model::{Inputs, LineLikeKind, LineStyle};
use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL, WARNING};

use super::define::{
    ClassicBand, ClassicIndicatorSpec, ClassicInput, ClassicLevel, ClassicOutput, ClassicPlot,
    flag, len, num, text,
};
use super::math::{
    atr, change, closes, ema, highest, highs, js_max, js_min, linreg, lowest, lows, map, mean_dev,
    opens, percent_rank, rma, roc, rsi, shift, sma, source_values, stdev, stoch, sub, sum, swma,
    wma, zip,
};
use super::shared::{
    BASE_TRANSPARENCY, BoundedOptions, CenteredOptions, DirectionInk, MomentumInk, Parts, RampInk,
    SETTINGS, STYLE, VALUE_TRANSPARENCY, ZONE_TRANSPARENCY, ZoneLevels, anchor_plot,
    baseline_gradient, bool_input, bounded_oscillator, centered_oscillator, color_input,
    float_input, length_input, momentum_column_colors, option_input, source_input, threshold_zones,
    transp,
};

const NAN: f64 = f64::NAN;

/// A message as its `(id, English text)` pair: keeps the catalog below one declaration per line.
type Text = (&'static str, &'static str);

fn m(t: Text) -> MessageLike {
    msg(t.0, t.1)
}

fn lenin(defval: f64, key: &str, title: Text, max: f64, tip: Text) -> ClassicInput {
    length_input(defval, key, m(title), max, Some(m(tip)))
}

fn srcin(defval: &str, key: &str, title: Text, tip: Text) -> ClassicInput {
    source_input(defval, key, m(title), Some(m(tip)))
}

fn fltin(
    key: &str,
    title: Text,
    defval: f64,
    min: f64,
    max: f64,
    step: f64,
    tip: Text,
) -> ClassicInput {
    float_input(key, m(title), defval, min, max, step, Some(m(tip)))
}

fn optin(key: &str, title: Text, defval: &str, options: &[&str], tip: Text) -> ClassicInput {
    option_input(key, m(title), defval, options, Some(m(tip)))
}

fn boolin(key: &str, title: Text, defval: bool, group: &str, tip: Text) -> ClassicInput {
    bool_input(key, m(title), defval, group, Some(m(tip)))
}

fn clr(defval: &str, key: &str, title: Text, tip: Text) -> ClassicInput {
    color_input(defval, m(title), key, Some(m(tip)))
}

fn with_min(mut input: ClassicInput, min: f64) -> ClassicInput {
    input.min = Some(min);
    input
}

fn level(
    key: &str,
    price: f64,
    color: impl Into<String>,
    style: LineStyle,
    title: Text,
) -> ClassicLevel {
    ClassicLevel::new(key, price)
        .color(color)
        .line_style(style)
        .title(m(title))
}

fn plot(key: &str, title: Text, values: Vec<f64>, color: impl Into<String>) -> ClassicPlot {
    ClassicPlot::new(key, m(title), values, color)
}

fn columns(
    key: &str,
    title: Text,
    values: Vec<f64>,
    color: impl Into<String>,
    colors: Vec<Option<String>>,
) -> ClassicPlot {
    plot(key, title, values, color)
        .kind(LineLikeKind::Columns)
        .colors(colors)
        .base(0.0)
}

fn from_parts(parts: Parts) -> ClassicOutput {
    ClassicOutput {
        plots: parts.plots,
        bands: parts.bands,
        levels: parts.levels,
        ..Default::default()
    }
}

/// Overbought / oversold thresholds, as every bounded oscillator declares them.
fn threshold_inputs(
    ob_default: f64,
    os_default: f64,
    min: f64,
    max: f64,
    titles: Option<(Text, Text)>,
) -> Vec<ClassicInput> {
    let (ob_title, os_title) = titles.unwrap_or((
        ("indicator.threshold.overbought", "Overbought Level"),
        ("indicator.threshold.oversold", "Oversold Level"),
    ));
    vec![
        fltin(
            "overbought",
            ob_title,
            ob_default,
            min,
            max,
            1.0,
            (
                "indicator.threshold.overbought.tooltip",
                "Level above which the oscillator is considered overbought.",
            ),
        ),
        fltin(
            "oversold",
            os_title,
            os_default,
            min,
            max,
            1.0,
            (
                "indicator.threshold.oversold.tooltip",
                "Level below which the oscillator is considered oversold.",
            ),
        ),
    ]
}

/// The common 0..100 thresholds with the default titles.
fn thresholds(ob_default: f64, os_default: f64) -> Vec<ClassicInput> {
    threshold_inputs(ob_default, os_default, 0.0, 100.0, None)
}

/// A [`BoundedOptions`] with every optional part at its default.
fn bounded(
    key: &str,
    title: Text,
    values: Vec<f64>,
    (overbought, oversold): (f64, f64),
    (scale_top, scale_bottom): (f64, f64),
    ink: RampInk,
    level_ink: String,
) -> BoundedOptions {
    BoundedOptions {
        key: key.to_string(),
        title: m(title),
        values,
        overbought,
        oversold,
        scale_top,
        scale_bottom,
        ink,
        level_ink,
        width: None,
        zone_transparency: None,
        midline: None,
        band_fill: None,
        midpoint_wash: false,
        level_titles: None,
    }
}

/// The oversold / mid / overbought inks every bounded oscillator reads.
fn ramp_ink(inputs: &Inputs, low: (&str, &str), mid: &str, high: (&str, &str)) -> RampInk {
    RampInk {
        low: text(inputs, low.0, low.1),
        mid: text(inputs, "color", mid),
        high: text(inputs, high.0, high.1),
    }
}

/// The bullish / bearish / neutral inks every centred oscillator reads.
fn direction_ink(inputs: &Inputs, neutral: String) -> DirectionInk {
    DirectionInk {
        bull: text(inputs, "bullColor", BULLISH),
        bear: text(inputs, "bearColor", BEARISH),
        neutral,
    }
}

fn rsi_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            14.0,
            "length",
            ("indicator.rsi.length", "Length"),
            5000.0,
            (
                "indicator.rsi.length.tooltip",
                "Number of bars used in the Wilder smoothing of average gains and losses.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.rsi.source", "Source"),
            (
                "indicator.rsi.source.tooltip",
                "Price series the RSI is calculated on.",
            ),
        ),
    ];
    inputs.extend(threshold_inputs(
        70.0,
        30.0,
        0.0,
        100.0,
        Some((
            ("indicator.rsi.overbought", "Overbought"),
            ("indicator.rsi.oversold", "Oversold"),
        )),
    ));
    inputs.extend([
        clr(
            NEUTRAL,
            "color",
            ("indicator.rsi.color", "RSI"),
            ("indicator.rsi.color.tooltip", "Color of the RSI line at the midline. Toward either threshold it blends into that threshold’s color."),
        ),
        clr(
            BULLISH,
            "oversoldColor",
            ("indicator.rsi.oversoldColor", "Oversold"),
            ("indicator.rsi.oversoldColor.tooltip", "Color of the line and zone shading at and below the oversold level."),
        ),
        clr(
            BEARISH,
            "overboughtColor",
            ("indicator.rsi.overboughtColor", "Overbought"),
            ("indicator.rsi.overboughtColor.tooltip", "Color of the line and zone shading at and above the overbought level."),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.rsi.levelsColor", "Levels"),
            ("indicator.rsi.levelsColor.tooltip", "Color of the overbought, midline and oversold levels."),
        ),
        clr(
            &transp(NEUTRAL, 90.0),
            "fillColor",
            ("indicator.rsi.fillColor", "Band Fill"),
            ("indicator.rsi.fillColor.tooltip", "Color of the fill between the overbought and oversold levels."),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "rsi",
        msg("indicator.rsi.title", "Relative Strength Index"),
        false,
        inputs,
        |bars, inputs| {
            let values = rsi(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "length", 14),
            );
            let mut opts = bounded(
                "rsi",
                ("indicator.rsi.plot.rsi", "RSI"),
                values,
                (
                    num(inputs, "overbought", 70.0),
                    num(inputs, "oversold", 30.0),
                ),
                (100.0, 0.0),
                ramp_ink(
                    inputs,
                    ("oversoldColor", BULLISH),
                    NEUTRAL,
                    ("overboughtColor", BEARISH),
                ),
                text(inputs, "levelsColor", NEUTRAL),
            );
            opts.width = Some(2.0);
            opts.midline = Some(50.0);
            opts.band_fill = Some(text(inputs, "fillColor", &transp(NEUTRAL, 90.0)));
            from_parts(bounded_oscillator(opts))
        },
    )
    .short_title(msg("indicator.rsi.shortTitle", "RSI"))
}

/// %K / %D over a threshold band: the shape the two stochastics share.
fn stochastic_output(k: Vec<f64>, d: Vec<f64>, inputs: &Inputs) -> ClassicOutput {
    let overbought = num(inputs, "overbought", 80.0);
    let oversold = num(inputs, "oversold", 20.0);
    let n = k.len();
    ClassicOutput {
        plots: vec![
            plot(
                "k",
                ("indicator.stochastic.plot.k", "%K"),
                k,
                text(inputs, "kColor", INFO),
            ),
            plot(
                "d",
                ("indicator.stochastic.plot.d", "%D"),
                d,
                text(inputs, "dColor", WARNING),
            ),
            anchor_plot("obAnchor", "Overbought Anchor", n, overbought),
            anchor_plot("osAnchor", "Oversold Anchor", n, oversold),
        ],
        bands: vec![ClassicBand::new(
            "band",
            "obAnchor",
            "osAnchor",
            text(inputs, "fillColor", &transp(INFO, 90.0)),
        )],
        levels: vec![
            level(
                "overbought",
                overbought,
                NEUTRAL,
                LineStyle::Dashed,
                ("indicator.common.level.overbought", "Overbought"),
            ),
            level(
                "oversold",
                oversold,
                NEUTRAL,
                LineStyle::Dashed,
                ("indicator.common.level.oversold", "Oversold"),
            ),
        ],
        ..Default::default()
    }
}

fn stochastic_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            14.0,
            "kLength",
            ("indicator.stochastic.kLength", "%K Length"),
            5000.0,
            (
                "indicator.stochastic.kLength.tooltip",
                "Number of bars used for the highest high / lowest low range of the raw %K.",
            ),
        ),
        lenin(
            3.0,
            "kSmoothing",
            ("indicator.stochastic.kSmoothing", "%K Smoothing"),
            500.0,
            (
                "indicator.stochastic.kSmoothing.tooltip",
                "Simple moving average length applied to the raw %K. 1 gives the fast stochastic, 3 the classic slow stochastic.",
            ),
        ),
        lenin(
            3.0,
            "dLength",
            ("indicator.stochastic.dLength", "%D Smoothing"),
            500.0,
            (
                "indicator.stochastic.dLength.tooltip",
                "Simple moving average length of the %D signal line, applied to the smoothed %K.",
            ),
        ),
    ];
    inputs.extend(thresholds(80.0, 20.0));
    inputs.extend([
        clr(
            INFO,
            "kColor",
            ("indicator.stochastic.kColor", "%K"),
            (
                "indicator.stochastic.kColor.tooltip",
                "Color of the %K line.",
            ),
        ),
        clr(
            WARNING,
            "dColor",
            ("indicator.stochastic.dColor", "%D"),
            (
                "indicator.stochastic.dColor.tooltip",
                "Color of the %D signal line.",
            ),
        ),
        clr(
            &transp(INFO, 90.0),
            "fillColor",
            ("indicator.stochastic.fillColor", "Band Fill"),
            (
                "indicator.stochastic.fillColor.tooltip",
                "Fill color of the area between the overbought and oversold levels.",
            ),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "stochastic",
        msg("indicator.stochastic.title", "Stochastic"),
        false,
        inputs,
        |bars, inputs| {
            let k = sma(
                &stoch(
                    &closes(bars),
                    &highs(bars),
                    &lows(bars),
                    len(inputs, "kLength", 14),
                ),
                len(inputs, "kSmoothing", 3),
            );
            let d = sma(&k, len(inputs, "dLength", 3));
            stochastic_output(k, d, inputs)
        },
    )
    .short_title(msg("indicator.stochastic.shortTitle", "Stoch"))
}

fn stochastic_rsi_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        srcin(
            "Close",
            "source",
            ("indicator.stochasticRsi.source", "RSI Source"),
            (
                "indicator.stochasticRsi.source.tooltip",
                "Price series the RSI is computed on.",
            ),
        ),
        lenin(
            14.0,
            "rsiLength",
            ("indicator.stochasticRsi.rsiLength", "RSI Length"),
            5000.0,
            (
                "indicator.stochasticRsi.rsiLength.tooltip",
                "Lookback length of the Wilder RSI.",
            ),
        ),
        lenin(
            14.0,
            "stochLength",
            ("indicator.stochasticRsi.stochLength", "Stochastic Length"),
            5000.0,
            (
                "indicator.stochasticRsi.stochLength.tooltip",
                "Lookback over which the highest and lowest RSI values are taken.",
            ),
        ),
        lenin(
            3.0,
            "kSmoothing",
            ("indicator.stochasticRsi.kSmoothing", "%K Smoothing"),
            500.0,
            (
                "indicator.stochasticRsi.kSmoothing.tooltip",
                "Simple moving average length applied to the stochastic of RSI to form %K.",
            ),
        ),
        lenin(
            3.0,
            "dLength",
            ("indicator.stochasticRsi.dLength", "%D Smoothing"),
            500.0,
            (
                "indicator.stochasticRsi.dLength.tooltip",
                "Simple moving average length applied to %K to form the %D signal line.",
            ),
        ),
    ];
    inputs.extend(thresholds(80.0, 20.0));
    inputs.extend([
        clr(
            INFO,
            "kColor",
            ("indicator.stochasticRsi.kColor", "%K Color"),
            (
                "indicator.stochasticRsi.kColor.tooltip",
                "Color of the %K line.",
            ),
        ),
        clr(
            WARNING,
            "dColor",
            ("indicator.stochasticRsi.dColor", "%D Color"),
            (
                "indicator.stochasticRsi.dColor.tooltip",
                "Color of the %D signal line.",
            ),
        ),
        clr(
            &transp(INFO, 90.0),
            "fillColor",
            ("indicator.stochasticRsi.fillColor", "Zone Fill"),
            (
                "indicator.stochasticRsi.fillColor.tooltip",
                "Fill color of the area between the overbought and oversold levels.",
            ),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "stochastic-rsi",
        msg("indicator.stochasticRsi.title", "Stochastic RSI"),
        false,
        inputs,
        |bars, inputs| {
            let r = rsi(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "rsiLength", 14),
            );
            let k = sma(
                &stoch(&r, &r, &r, len(inputs, "stochLength", 14)),
                len(inputs, "kSmoothing", 3),
            );
            let d = sma(&k, len(inputs, "dLength", 3));
            stochastic_output(k, d, inputs)
        },
    )
    .short_title(msg("indicator.stochasticRsi.shortTitle", "Stoch RSI"))
}

fn macd_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            12.0,
            "fastLength",
            ("indicator.macd.fastLength", "Fast Length"),
            5000.0,
            (
                "indicator.macd.fastLength.tooltip",
                "Length of the fast EMA.",
            ),
        ),
        lenin(
            26.0,
            "slowLength",
            ("indicator.macd.slowLength", "Slow Length"),
            5000.0,
            (
                "indicator.macd.slowLength.tooltip",
                "Length of the slow EMA.",
            ),
        ),
        lenin(
            9.0,
            "signalLength",
            ("indicator.macd.signalLength", "Signal Smoothing"),
            500.0,
            (
                "indicator.macd.signalLength.tooltip",
                "Length of the EMA applied to the MACD line to obtain the signal line.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.macd.source", "Source"),
            (
                "indicator.macd.source.tooltip",
                "Price series used to compute the fast and slow EMAs.",
            ),
        ),
        clr(
            INFO,
            "color",
            ("indicator.macd.color", "MACD Line"),
            ("indicator.macd.color.tooltip", "Color of the MACD line."),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.macd.signalColor", "Signal Line"),
            (
                "indicator.macd.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "growAboveColor",
            ("indicator.macd.growAboveColor", "Grow Above"),
            (
                "indicator.macd.growAboveColor.tooltip",
                "Histogram color when above zero and rising.",
            ),
        ),
        clr(
            &transp(BULLISH, 50.0),
            "fallAboveColor",
            ("indicator.macd.fallAboveColor", "Fall Above"),
            (
                "indicator.macd.fallAboveColor.tooltip",
                "Histogram color when above zero and falling.",
            ),
        ),
        clr(
            &transp(BEARISH, 50.0),
            "growBelowColor",
            ("indicator.macd.growBelowColor", "Grow Below"),
            (
                "indicator.macd.growBelowColor.tooltip",
                "Histogram color when below zero and rising.",
            ),
        ),
        clr(
            BEARISH,
            "fallBelowColor",
            ("indicator.macd.fallBelowColor", "Fall Below"),
            (
                "indicator.macd.fallBelowColor.tooltip",
                "Histogram color when below zero and falling.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "macd",
        msg("indicator.macd.title", "MACD"),
        false,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let line = sub(
                &ema(&src, len(inputs, "fastLength", 12)),
                &ema(&src, len(inputs, "slowLength", 26)),
            );
            let signal = ema(&line, len(inputs, "signalLength", 9));
            let hist = sub(&line, &signal);
            let colors = momentum_column_colors(
                &hist,
                &MomentumInk {
                    grow_above: text(inputs, "growAboveColor", BULLISH),
                    fall_above: text(inputs, "fallAboveColor", &transp(BULLISH, 50.0)),
                    grow_below: text(inputs, "growBelowColor", &transp(BEARISH, 50.0)),
                    fall_below: text(inputs, "fallBelowColor", BEARISH),
                },
            );
            ClassicOutput {
                plots: vec![
                    columns(
                        "hist",
                        ("indicator.macd.plot.hist", "Histogram"),
                        hist,
                        BULLISH,
                        colors,
                    ),
                    plot(
                        "macd",
                        ("indicator.macd.plot.macd", "MACD"),
                        line,
                        text(inputs, "color", INFO),
                    ),
                    plot(
                        "signal",
                        ("indicator.macd.plot.signal", "Signal"),
                        signal,
                        text(inputs, "signalColor", WARNING),
                    ),
                ],
                levels: vec![level(
                    "zero",
                    0.0,
                    NEUTRAL,
                    LineStyle::Dashed,
                    ("indicator.macd.level.zero", "Zero Line"),
                )],
                ..Default::default()
            }
        },
    )
}

fn ppo_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        srcin(
            "Close",
            "source",
            ("indicator.ppo.source", "Source"),
            (
                "indicator.ppo.source.tooltip",
                "Price series fed into the fast and slow averages.",
            ),
        ),
        lenin(
            12.0,
            "fastLength",
            ("indicator.ppo.fastLength", "Fast Length"),
            5000.0,
            (
                "indicator.ppo.fastLength.tooltip",
                "Length of the fast moving average.",
            ),
        ),
        lenin(
            26.0,
            "slowLength",
            ("indicator.ppo.slowLength", "Slow Length"),
            5000.0,
            (
                "indicator.ppo.slowLength.tooltip",
                "Length of the slow moving average. The spread is divided by it and scaled to a percentage of price.",
            ),
        ),
        optin(
            "maType",
            ("indicator.ppo.maType", "Average Type"),
            "EMA",
            &["EMA", "SMA"],
            (
                "indicator.ppo.maType.tooltip",
                "Moving average type used for the fast and slow averages.",
            ),
        ),
        lenin(
            9.0,
            "signalLength",
            ("indicator.ppo.signalLength", "Signal Smoothing"),
            500.0,
            (
                "indicator.ppo.signalLength.tooltip",
                "Length of the signal line that smooths the PPO line.",
            ),
        ),
        optin(
            "signalType",
            ("indicator.ppo.signalType", "Signal Type"),
            "EMA",
            &["EMA", "SMA"],
            (
                "indicator.ppo.signalType.tooltip",
                "Moving average type used for the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.ppo.bullColor", "Bullish"),
            (
                "indicator.ppo.bullColor.tooltip",
                "Color of the PPO line, gradient fill and histogram above the zero line.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.ppo.bearColor", "Bearish"),
            (
                "indicator.ppo.bearColor.tooltip",
                "Color of the PPO line, gradient fill and histogram below the zero line.",
            ),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.ppo.signalColor", "Signal Line"),
            (
                "indicator.ppo.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
        boolin(
            "showHistogram",
            ("indicator.ppo.showHistogram", "Show Histogram"),
            true,
            STYLE,
            (
                "indicator.ppo.showHistogram.tooltip",
                "Displays the histogram (PPO minus signal line) as columns. Bright columns mark a spread still widening, faded ones a spread narrowing back.",
            ),
        ),
        boolin(
            "gradientFill",
            ("indicator.ppo.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            (
                "indicator.ppo.gradientFill.tooltip",
                "Fills the area between the zero line and the PPO line with a vertical gradient.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "ppo",
        msg("indicator.ppo.title", "Percentage Price Oscillator"),
        false,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let average = |kind: &str, v: &[f64], length: usize| {
                if kind == "SMA" {
                    sma(v, length)
                } else {
                    ema(v, length)
                }
            };
            let kind = text(inputs, "maType", "EMA");
            let fast = average(&kind, &src, len(inputs, "fastLength", 12));
            let slow = average(&kind, &src, len(inputs, "slowLength", 26));
            let line = zip(&fast, &slow, |f, s| {
                if s == 0.0 { NAN } else { ((f - s) / s) * 100.0 }
            });
            let signal = average(
                &text(inputs, "signalType", "EMA"),
                &line,
                len(inputs, "signalLength", 9),
            );
            let hist = sub(&line, &signal);
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let mut opts = CenteredOptions::new(
                "ppo",
                msg("indicator.ppo.plot.ppo", "PPO Line"),
                line,
                DirectionInk {
                    bull: bull.clone(),
                    bear: bear.clone(),
                    neutral: bear.clone(),
                },
                transp(NEUTRAL, 90.0),
            );
            opts.fill = flag(inputs, "gradientFill", true);
            opts.extra_plots = vec![plot(
                "signal",
                ("indicator.ppo.plot.signal", "Signal Line"),
                signal,
                text(inputs, "signalColor", WARNING),
            )];
            let mut out = from_parts(centered_oscillator(opts));
            if flag(inputs, "showHistogram", true) {
                // Bright while the spread between the averages widens, faded once it narrows again.
                let colors: Vec<Option<String>> = hist
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        if !x.is_finite() {
                            return None;
                        }
                        let prev = if i > 0 && hist[i - 1].is_finite() {
                            hist[i - 1]
                        } else {
                            x
                        };
                        let ink = if x >= 0.0 { &bull } else { &bear };
                        let expanding = if x >= 0.0 { x > prev } else { x < prev };
                        Some(transp(ink, if expanding { 40.0 } else { 70.0 }))
                    })
                    .collect();
                out.plots.insert(
                    0,
                    columns(
                        "hist",
                        ("indicator.ppo.plot.hist", "Histogram"),
                        hist,
                        bull,
                        colors,
                    ),
                );
            }
            out
        },
    )
    .short_title(msg("indicator.ppo.shortTitle", "PPO"))
}

fn awesome_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        clr(
            BULLISH,
            "risingColor",
            ("indicator.awesomeOscillator.risingColor", "Rising"),
            (
                "indicator.awesomeOscillator.risingColor.tooltip",
                "Histogram color when the oscillator is higher than on the previous bar.",
            ),
        ),
        clr(
            BEARISH,
            "fallingColor",
            ("indicator.awesomeOscillator.fallingColor", "Falling"),
            (
                "indicator.awesomeOscillator.fallingColor.tooltip",
                "Histogram color when the oscillator is lower than or equal to the previous bar.",
            ),
        ),
        clr(
            NEUTRAL,
            "zeroColor",
            ("indicator.awesomeOscillator.zeroColor", "Zero Line"),
            (
                "indicator.awesomeOscillator.zeroColor.tooltip",
                "Color of the zero reference line.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "awesome-oscillator",
        msg("indicator.awesomeOscillator.title", "Awesome Oscillator"),
        false,
        inputs,
        |bars, inputs| {
            let hl2 = source_values(bars, "HL2");
            let ao = sub(&sma(&hl2, 5), &sma(&hl2, 34));
            let rising = text(inputs, "risingColor", BULLISH);
            let falling = text(inputs, "fallingColor", BEARISH);
            let colors: Vec<Option<String>> = ao
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    if !x.is_finite() {
                        return None;
                    }
                    let prev = if i > 0 { ao[i - 1] } else { NAN };
                    Some(if prev.is_finite() && x > prev {
                        rising.clone()
                    } else {
                        falling.clone()
                    })
                })
                .collect();
            ClassicOutput {
                plots: vec![columns(
                    "ao",
                    ("indicator.awesomeOscillator.plot.ao", "AO"),
                    ao,
                    rising,
                    colors,
                )],
                levels: vec![level(
                    "zero",
                    0.0,
                    text(inputs, "zeroColor", NEUTRAL),
                    LineStyle::Dashed,
                    ("indicator.awesomeOscillator.level.zero", "Zero Line"),
                )],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.awesomeOscillator.shortTitle", "AO"))
}

/// Lambert's scaling constant.
const CCI_SCALING: f64 = 0.015;

fn cci_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            20.0,
            "length",
            ("indicator.commodityChannelIndex.length", "Length"),
            5000.0,
            (
                "indicator.commodityChannelIndex.length.tooltip",
                "Number of bars used for the moving average and the mean absolute deviation of the source.",
            ),
        ),
        srcin(
            "HLC3",
            "source",
            ("indicator.commodityChannelIndex.source", "Source"),
            (
                "indicator.commodityChannelIndex.source.tooltip",
                "Price series measured by the CCI; the typical price hlc3 is the classic choice.",
            ),
        ),
        fltin(
            "upper",
            ("indicator.commodityChannelIndex.upper", "Upper Level"),
            100.0,
            -1000.0,
            1000.0,
            10.0,
            (
                "indicator.commodityChannelIndex.upper.tooltip",
                "Overbought reference level, +100 classically.",
            ),
        ),
        fltin(
            "lower",
            ("indicator.commodityChannelIndex.lower", "Lower Level"),
            -100.0,
            -1000.0,
            1000.0,
            10.0,
            (
                "indicator.commodityChannelIndex.lower.tooltip",
                "Oversold reference level, −100 classically.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.commodityChannelIndex.color", "CCI Line"),
            (
                "indicator.commodityChannelIndex.color.tooltip",
                "Color of the CCI line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.commodityChannelIndex.bullColor", "Bullish"),
            (
                "indicator.commodityChannelIndex.bullColor.tooltip",
                "Line and fill color while the CCI reads above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.commodityChannelIndex.bearColor", "Bearish"),
            (
                "indicator.commodityChannelIndex.bearColor.tooltip",
                "Line and fill color while the CCI reads below zero.",
            ),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.commodityChannelIndex.levelsColor", "Levels"),
            (
                "indicator.commodityChannelIndex.levelsColor.tooltip",
                "Color of the upper, lower and zero reference lines.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "commodity-channel-index",
        msg(
            "indicator.commodityChannelIndex.title",
            "Commodity Channel Index",
        ),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 20);
            let src = source_values(bars, &text(inputs, "source", "HLC3"));
            let basis = sma(&src, length);
            let dev = mean_dev(&src, length);
            let values: Vec<f64> = src
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    let (b, d) = (basis[i], dev[i]);
                    if !b.is_finite() || !d.is_finite() {
                        return NAN;
                    }
                    if d == 0.0 {
                        0.0
                    } else {
                        (x - b) / (CCI_SCALING * d)
                    }
                })
                .collect();
            let level_ink = text(inputs, "levelsColor", NEUTRAL);
            let mut opts = CenteredOptions::new(
                "cci",
                msg("indicator.commodityChannelIndex.plot.cci", "CCI"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                level_ink.clone(),
            );
            opts.zero_line_style = Some(LineStyle::Dotted);
            opts.extra_levels = vec![
                level(
                    "upper",
                    num(inputs, "upper", 100.0),
                    level_ink.clone(),
                    LineStyle::Dashed,
                    ("indicator.commodityChannelIndex.level.upper", "Upper Level"),
                ),
                level(
                    "lower",
                    num(inputs, "lower", -100.0),
                    level_ink,
                    LineStyle::Dashed,
                    ("indicator.commodityChannelIndex.level.lower", "Lower Level"),
                ),
            ];
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.commodityChannelIndex.shortTitle", "CCI"))
}

fn williams_r_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            14.0,
            "length",
            ("indicator.williamsPercentR.length", "Length"),
            5000.0,
            (
                "indicator.williamsPercentR.length.tooltip",
                "Lookback length of the highest high / lowest low range. 14 is the classic setting.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.williamsPercentR.source", "Source"),
            (
                "indicator.williamsPercentR.source.tooltip",
                "Series located within the lookback range. Close is the classic choice.",
            ),
        ),
    ];
    inputs.extend(threshold_inputs(-20.0, -80.0, -100.0, 0.0, None));
    inputs.extend([
        clr(
            NEUTRAL,
            "color",
            ("indicator.williamsPercentR.color", "%R Line"),
            ("indicator.williamsPercentR.color.tooltip", "Color of the %R line between the levels. Toward either level it blends into that level’s color."),
        ),
        clr(
            BEARISH,
            "overboughtColor",
            ("indicator.williamsPercentR.overboughtColor", "Overbought"),
            ("indicator.williamsPercentR.overboughtColor.tooltip", "Color of the line and zone shading at and above the overbought level."),
        ),
        clr(
            BULLISH,
            "oversoldColor",
            ("indicator.williamsPercentR.oversoldColor", "Oversold"),
            ("indicator.williamsPercentR.oversoldColor.tooltip", "Color of the line and zone shading at and below the oversold level."),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "williams-percent-r",
        msg("indicator.williamsPercentR.title", "Williams %R"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 14);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let hh = highest(&highs(bars), length);
            let ll = lowest(&lows(bars), length);
            // A flat lookback range leaves %R undefined: a gap, never a fabricated midpoint.
            let values: Vec<f64> = src
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    let (h, l) = (hh[i], ll[i]);
                    if h.is_finite() && l.is_finite() && h > l {
                        (-100.0 * (h - x)) / (h - l)
                    } else {
                        NAN
                    }
                })
                .collect();
            let opts = bounded(
                "wr",
                ("indicator.williamsPercentR.plot.wr", "%R"),
                values,
                (
                    num(inputs, "overbought", -20.0),
                    num(inputs, "oversold", -80.0),
                ),
                (0.0, -100.0),
                ramp_ink(
                    inputs,
                    ("oversoldColor", BULLISH),
                    NEUTRAL,
                    ("overboughtColor", BEARISH),
                ),
                NEUTRAL.to_string(),
            );
            from_parts(bounded_oscillator(opts))
        },
    )
    .short_title(msg("indicator.williamsPercentR.shortTitle", "%R"))
}

fn cmo_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            9.0,
            "length",
            ("indicator.chandeMomentumOscillator.length", "Length"),
            5000.0,
            (
                "indicator.chandeMomentumOscillator.length.tooltip",
                "Number of bars over which up moves and down moves are summed.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.chandeMomentumOscillator.source", "Source"),
            (
                "indicator.chandeMomentumOscillator.source.tooltip",
                "Price series used to measure bar-to-bar changes.",
            ),
        ),
        fltin(
            "overbought",
            (
                "indicator.chandeMomentumOscillator.overbought",
                "Overbought Level",
            ),
            50.0,
            0.0,
            100.0,
            1.0,
            (
                "indicator.chandeMomentumOscillator.overbought.tooltip",
                "Level above which the oscillator is considered overbought.",
            ),
        ),
        fltin(
            "oversold",
            (
                "indicator.chandeMomentumOscillator.oversold",
                "Oversold Level",
            ),
            -50.0,
            -100.0,
            0.0,
            1.0,
            (
                "indicator.chandeMomentumOscillator.oversold.tooltip",
                "Level below which the oscillator is considered oversold.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.chandeMomentumOscillator.color", "CMO Line"),
            (
                "indicator.chandeMomentumOscillator.color.tooltip",
                "Color of the CMO line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.chandeMomentumOscillator.bullColor", "Bullish"),
            (
                "indicator.chandeMomentumOscillator.bullColor.tooltip",
                "Line and fill color while momentum is positive, and shading of the oversold zone.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.chandeMomentumOscillator.bearColor", "Bearish"),
            (
                "indicator.chandeMomentumOscillator.bearColor.tooltip",
                "Line and fill color while momentum is negative, and shading of the overbought zone.",
            ),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.chandeMomentumOscillator.levelsColor", "Levels"),
            (
                "indicator.chandeMomentumOscillator.levelsColor.tooltip",
                "Color of the overbought, oversold, and zero lines.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "chande-momentum-oscillator",
        msg(
            "indicator.chandeMomentumOscillator.title",
            "Chande Momentum Oscillator",
        ),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 9);
            let d = change(&source_values(bars, &text(inputs, "source", "Close")), 1);
            let up = sum(&map(&d, |x| js_max(x, 0.0)), length);
            let down = sum(&map(&d, |x| js_max(-x, 0.0)), length);
            let values = zip(&up, &down, |u, w| {
                if u + w == 0.0 {
                    0.0
                } else {
                    (100.0 * (u - w)) / (u + w)
                }
            });
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let level_ink = text(inputs, "levelsColor", NEUTRAL);
            let overbought = num(inputs, "overbought", 50.0);
            let oversold = num(inputs, "oversold", -50.0);
            let n = values.len();
            let mut opts = CenteredOptions::new(
                "cmo",
                msg("indicator.chandeMomentumOscillator.plot.cmo", "CMO"),
                values,
                DirectionInk {
                    bull: bull.clone(),
                    bear: bear.clone(),
                    neutral: text(inputs, "color", NEUTRAL),
                },
                level_ink.clone(),
            );
            opts.zero_line_style = Some(LineStyle::Dotted);
            opts.extra_levels = vec![
                level(
                    "overbought",
                    overbought,
                    level_ink.clone(),
                    LineStyle::Dashed,
                    (
                        "indicator.chandeMomentumOscillator.level.overbought",
                        "Overbought",
                    ),
                ),
                level(
                    "oversold",
                    oversold,
                    level_ink,
                    LineStyle::Dashed,
                    (
                        "indicator.chandeMomentumOscillator.level.oversold",
                        "Oversold",
                    ),
                ),
            ];
            let mut out = from_parts(centered_oscillator(opts));
            let zones = threshold_zones(
                n,
                ZoneLevels {
                    overbought,
                    oversold,
                    top: 100.0,
                    bottom: -100.0,
                },
                &bear,
                &bull,
                ZONE_TRANSPARENCY,
            );
            out.plots.extend(zones.plots);
            out.bands.extend(zones.bands);
            out
        },
    )
    .short_title(msg("indicator.chandeMomentumOscillator.shortTitle", "CMO"))
}

fn connors_rsi_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            3.0,
            "rsiLength",
            ("indicator.connorsRsi.rsiLength", "RSI Length"),
            5000.0,
            (
                "indicator.connorsRsi.rsiLength.tooltip",
                "Length of the Wilder RSI applied to the closing price.",
            ),
        ),
        lenin(
            2.0,
            "streakLength",
            ("indicator.connorsRsi.streakLength", "Streak RSI Length"),
            5000.0,
            (
                "indicator.connorsRsi.streakLength.tooltip",
                "Length of the Wilder RSI applied to the up/down close streak.",
            ),
        ),
        lenin(
            100.0,
            "rankLength",
            ("indicator.connorsRsi.rankLength", "Percent Rank Length"),
            5000.0,
            (
                "indicator.connorsRsi.rankLength.tooltip",
                "Lookback of the percent rank applied to the 1-bar rate of change of the closing price.",
            ),
        ),
    ];
    inputs.extend(thresholds(90.0, 10.0));
    inputs.extend([
        clr(
            NEUTRAL,
            "color",
            ("indicator.connorsRsi.color", "CRSI Line"),
            (
                "indicator.connorsRsi.color.tooltip",
                "Color of the Connors RSI line.",
            ),
        ),
        clr(
            BULLISH,
            "oversoldColor",
            ("indicator.connorsRsi.oversoldColor", "Oversold"),
            (
                "indicator.connorsRsi.oversoldColor.tooltip",
                "Line, fill and zone color toward and below the oversold level.",
            ),
        ),
        clr(
            BEARISH,
            "overboughtColor",
            ("indicator.connorsRsi.overboughtColor", "Overbought"),
            (
                "indicator.connorsRsi.overboughtColor.tooltip",
                "Line, fill and zone color toward and above the overbought level.",
            ),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.connorsRsi.levelsColor", "Levels"),
            (
                "indicator.connorsRsi.levelsColor.tooltip",
                "Color of the overbought and oversold levels.",
            ),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "connors-rsi",
        msg("indicator.connorsRsi.title", "Connors RSI"),
        false,
        inputs,
        |bars, inputs| {
            let c = closes(bars);
            // Signed run length of consecutive closes in one direction.
            let mut streak = vec![0.0_f64; c.len()];
            for i in 1..c.len() {
                let d = c[i] - c[i - 1];
                streak[i] = if d > 0.0 {
                    streak[i - 1].max(0.0) + 1.0
                } else if d < 0.0 {
                    streak[i - 1].min(0.0) - 1.0
                } else {
                    0.0
                };
            }
            let a = rsi(&c, len(inputs, "rsiLength", 3));
            let b = rsi(&streak, len(inputs, "streakLength", 2));
            let r = percent_rank(&roc(&c, 1), len(inputs, "rankLength", 100));
            let values: Vec<f64> = (0..c.len())
                .map(|i| {
                    let (x, y, z) = (a[i], b[i], r[i]);
                    if x.is_finite() && y.is_finite() && z.is_finite() {
                        (x + y + z) / 3.0
                    } else {
                        NAN
                    }
                })
                .collect();
            let mut opts = bounded(
                "crsi",
                ("indicator.connorsRsi.plot.crsi", "Connors RSI"),
                values,
                (
                    num(inputs, "overbought", 90.0),
                    num(inputs, "oversold", 10.0),
                ),
                (100.0, 0.0),
                ramp_ink(
                    inputs,
                    ("oversoldColor", BULLISH),
                    NEUTRAL,
                    ("overboughtColor", BEARISH),
                ),
                text(inputs, "levelsColor", NEUTRAL),
            );
            opts.midpoint_wash = true;
            from_parts(bounded_oscillator(opts))
        },
    )
    .short_title(msg("indicator.connorsRsi.shortTitle", "CRSI"))
}

fn fisher_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            9.0,
            "length",
            ("indicator.fisherTransform.length", "Length"),
            5000.0,
            (
                "indicator.fisherTransform.length.tooltip",
                "Lookback period used to locate the median price hl2 within its highest-lowest range. Ehlers' original length is 10; 9 is the common platform default.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.fisherTransform.color", "Fisher Line"),
            (
                "indicator.fisherTransform.color.tooltip",
                "Color of the Fisher line when it sits exactly on its trigger.",
            ),
        ),
        clr(
            NEUTRAL,
            "triggerColor",
            ("indicator.fisherTransform.triggerColor", "Trigger Line"),
            (
                "indicator.fisherTransform.triggerColor.tooltip",
                "Color of the trigger line, the Fisher value delayed one bar.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.fisherTransform.bullColor", "Bullish"),
            (
                "indicator.fisherTransform.bullColor.tooltip",
                "Color of the Fisher line and fill while it is above the trigger.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.fisherTransform.bearColor", "Bearish"),
            (
                "indicator.fisherTransform.bearColor.tooltip",
                "Color of the Fisher line and fill while it is below the trigger.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "fisher-transform",
        msg("indicator.fisherTransform.title", "Fisher Transform"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 9);
            let hl2 = source_values(bars, "HL2");
            let hi = highest(&hl2, length);
            let lo = lowest(&hl2, length);
            let n = bars.len();
            let mut out = vec![NAN; n];
            let mut position = 0.0_f64;
            let mut value = 0.0_f64;
            for i in 0..n {
                let (h, l) = (hi[i], lo[i]);
                if !h.is_finite() || !l.is_finite() {
                    continue;
                }
                // A flat range contributes a 0 position term so the recursion stays defined.
                let raw = if h > l {
                    (hl2[i] - l) / (h - l) - 0.5
                } else {
                    0.0
                };
                position = 0.66 * raw + 0.67 * position;
                position = if position > 0.99 {
                    0.999
                } else if position < -0.99 {
                    -0.999
                } else {
                    position
                };
                value = 0.5 * ((1.0 + position) / (1.0 - position)).ln() + 0.5 * value;
                out[i] = value;
            }
            let trigger = shift(&out, 1);
            let neutral = text(inputs, "color", NEUTRAL);
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let ink_of = |i: usize| -> &String {
                let (f, t) = (out[i], trigger[i]);
                if !t.is_finite() {
                    &neutral
                } else if f > t {
                    &bull
                } else if f < t {
                    &bear
                } else {
                    &neutral
                }
            };
            let line_colors: Vec<Option<String>> = out
                .iter()
                .enumerate()
                .map(|(i, x)| x.is_finite().then(|| ink_of(i).clone()))
                .collect();
            let band_colors: Vec<Option<String>> = out
                .iter()
                .enumerate()
                .map(|(i, x)| x.is_finite().then(|| transp(ink_of(i), 80.0)))
                .collect();
            ClassicOutput {
                plots: vec![
                    plot(
                        "fisher",
                        ("indicator.fisherTransform.plot.fisher", "Fisher"),
                        out.clone(),
                        neutral.clone(),
                    )
                    .width(2.0)
                    .colors(line_colors),
                    plot(
                        "trigger",
                        ("indicator.fisherTransform.plot.trigger", "Trigger"),
                        trigger.clone(),
                        text(inputs, "triggerColor", NEUTRAL),
                    ),
                ],
                bands: vec![
                    ClassicBand::new("spread", "fisher", "trigger", transp(&neutral, 80.0))
                        .colors(band_colors),
                ],
                levels: vec![
                    level(
                        "upper",
                        1.5,
                        NEUTRAL,
                        LineStyle::Dashed,
                        ("indicator.fisherTransform.level.upper", "Upper Level"),
                    ),
                    level(
                        "lower",
                        -1.5,
                        NEUTRAL,
                        LineStyle::Dashed,
                        ("indicator.fisherTransform.level.lower", "Lower Level"),
                    ),
                    level(
                        "zero",
                        0.0,
                        NEUTRAL,
                        LineStyle::Dotted,
                        ("indicator.fisherTransform.level.zero", "Zero Line"),
                    ),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.fisherTransform.shortTitle", "Fisher"))
}

fn trix_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            18.0,
            "length",
            ("indicator.trix.length", "Length"),
            5000.0,
            (
                "indicator.trix.length.tooltip",
                "Length of each of the three EMA smoothing passes applied to the close price.",
            ),
        ),
        lenin(
            9.0,
            "signalLength",
            ("indicator.trix.signalLength", "Signal Length"),
            500.0,
            (
                "indicator.trix.signalLength.tooltip",
                "Length of the EMA applied to TRIX to form the signal line.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.trix.color", "TRIX Color"),
            ("indicator.trix.color.tooltip", "Color of the TRIX line."),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.trix.signalColor", "Signal Color"),
            (
                "indicator.trix.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.trix.bullColor", "Bullish"),
            (
                "indicator.trix.bullColor.tooltip",
                "Color of the TRIX line and its gradient fill while TRIX holds above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.trix.bearColor", "Bearish"),
            (
                "indicator.trix.bearColor.tooltip",
                "Color of the TRIX line and its gradient fill while TRIX holds below zero.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "trix",
        msg("indicator.trix.title", "TRIX"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 18);
            let smooth = ema(&ema(&ema(&closes(bars), length), length), length);
            let values = roc(&smooth, 1);
            let signal = ema(&values, len(inputs, "signalLength", 9));
            let mut opts = CenteredOptions::new(
                "trix",
                msg("indicator.trix.plot.trix", "TRIX"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                NEUTRAL,
            );
            opts.extra_plots = vec![plot(
                "signal",
                ("indicator.trix.plot.signal", "Signal"),
                signal,
                text(inputs, "signalColor", WARNING),
            )];
            from_parts(centered_oscillator(opts))
        },
    )
}

/// Double-smoothed momentum ratio: Blau's TSI core, in -1..1.
fn tsi_ratio(src: &[f64], long: usize, short: usize) -> Vec<f64> {
    let mom = change(src, 1);
    let numer = ema(&ema(&mom, long), short);
    let denom = ema(&ema(&map(&mom, f64::abs), long), short);
    zip(&numer, &denom, |a, b| if b == 0.0 { 0.0 } else { a / b })
}

fn tsi_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            25.0,
            "longLength",
            ("indicator.trueStrengthIndex.longLength", "Long Length"),
            5000.0,
            (
                "indicator.trueStrengthIndex.longLength.tooltip",
                "Length of the first (long) EMA applied to the 1-bar price change.",
            ),
        ),
        lenin(
            13.0,
            "shortLength",
            ("indicator.trueStrengthIndex.shortLength", "Short Length"),
            5000.0,
            (
                "indicator.trueStrengthIndex.shortLength.tooltip",
                "Length of the second (short) EMA applied to the long-smoothed price change.",
            ),
        ),
        lenin(
            13.0,
            "signalLength",
            ("indicator.trueStrengthIndex.signalLength", "Signal Length"),
            500.0,
            (
                "indicator.trueStrengthIndex.signalLength.tooltip",
                "Length of the EMA of TSI used as the signal line.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.trueStrengthIndex.color", "TSI"),
            (
                "indicator.trueStrengthIndex.color.tooltip",
                "Color of the TSI line.",
            ),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.trueStrengthIndex.signalColor", "Signal"),
            (
                "indicator.trueStrengthIndex.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.trueStrengthIndex.bullColor", "Bullish"),
            (
                "indicator.trueStrengthIndex.bullColor.tooltip",
                "Color of the TSI line and its gradient fill while TSI holds above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.trueStrengthIndex.bearColor", "Bearish"),
            (
                "indicator.trueStrengthIndex.bearColor.tooltip",
                "Color of the TSI line and its gradient fill while TSI holds below zero.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "true-strength-index",
        msg("indicator.trueStrengthIndex.title", "True Strength Index"),
        false,
        inputs,
        |bars, inputs| {
            let values = map(
                &tsi_ratio(
                    &closes(bars),
                    len(inputs, "longLength", 25),
                    len(inputs, "shortLength", 13),
                ),
                |x| 100.0 * x,
            );
            let signal = ema(&values, len(inputs, "signalLength", 13));
            let mut opts = CenteredOptions::new(
                "tsi",
                msg("indicator.trueStrengthIndex.plot.tsi", "TSI"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                NEUTRAL,
            );
            opts.extra_plots = vec![plot(
                "signal",
                ("indicator.trueStrengthIndex.plot.signal", "Signal"),
                signal,
                text(inputs, "signalColor", WARNING),
            )];
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.trueStrengthIndex.shortTitle", "TSI"))
}

const SMI_INDICATOR: &str = "Indicator";
const SMI_OSCILLATOR: &str = "Oscillator";

fn smi_ergodic_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            20.0,
            "longLength",
            ("indicator.smiErgodic.longLength", "Long Length"),
            5000.0,
            (
                "indicator.smiErgodic.longLength.tooltip",
                "Length of the first EMA smoothing applied to the momentum of the closing price.",
            ),
        ),
        lenin(
            5.0,
            "shortLength",
            ("indicator.smiErgodic.shortLength", "Short Length"),
            5000.0,
            (
                "indicator.smiErgodic.shortLength.tooltip",
                "Length of the second EMA smoothing applied to the momentum of the closing price.",
            ),
        ),
        lenin(
            5.0,
            "signalLength",
            ("indicator.smiErgodic.signalLength", "Signal Length"),
            500.0,
            (
                "indicator.smiErgodic.signalLength.tooltip",
                "Length of the EMA of the SMI that forms the signal line.",
            ),
        ),
        optin(
            "display",
            ("indicator.smiErgodic.display", "Display"),
            SMI_INDICATOR,
            &[SMI_INDICATOR, SMI_OSCILLATOR],
            (
                "indicator.smiErgodic.display.tooltip",
                "Indicator plots the SMI and its signal line. Oscillator plots the difference between them as a histogram.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.smiErgodic.color", "SMI"),
            (
                "indicator.smiErgodic.color.tooltip",
                "Color of the SMI line while it sits exactly on its signal line.",
            ),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.smiErgodic.signalColor", "Signal"),
            (
                "indicator.smiErgodic.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "growColor",
            ("indicator.smiErgodic.growColor", "Oscillator Grow"),
            (
                "indicator.smiErgodic.growColor.tooltip",
                "Color of the rising oscillator histogram, and of the SMI line while it leads its signal.",
            ),
        ),
        clr(
            BEARISH,
            "fallColor",
            ("indicator.smiErgodic.fallColor", "Oscillator Fall"),
            (
                "indicator.smiErgodic.fallColor.tooltip",
                "Color of the falling oscillator histogram, and of the SMI line while it lags its signal.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "smi-ergodic",
        msg("indicator.smiErgodic.title", "SMI Ergodic Oscillator"),
        false,
        inputs,
        |bars, inputs| {
            let smi = tsi_ratio(
                &closes(bars),
                len(inputs, "longLength", 20),
                len(inputs, "shortLength", 5),
            );
            let signal = ema(&smi, len(inputs, "signalLength", 5));
            let grow = text(inputs, "growColor", BULLISH);
            let fall = text(inputs, "fallColor", BEARISH);
            let zero = vec![level(
                "zero",
                0.0,
                transp(NEUTRAL, 50.0),
                LineStyle::Dashed,
                ("indicator.smiErgodic.level.zero", "Zero Line"),
            )];
            if text(inputs, "display", SMI_INDICATOR) == SMI_OSCILLATOR {
                let osc = sub(&smi, &signal);
                let colors: Vec<Option<String>> = osc
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        if !x.is_finite() {
                            return None;
                        }
                        let prev = if i > 0 && osc[i - 1].is_finite() {
                            osc[i - 1]
                        } else {
                            x
                        };
                        Some(if x > prev { grow.clone() } else { fall.clone() })
                    })
                    .collect();
                return ClassicOutput {
                    plots: vec![columns(
                        "osc",
                        ("indicator.smiErgodic.plot.osc", "Oscillator"),
                        osc,
                        grow,
                        colors,
                    )],
                    levels: zero,
                    ..Default::default()
                };
            }
            let neutral = text(inputs, "color", NEUTRAL);
            let ink_of = |i: usize| -> &String {
                let (a, b) = (smi[i], signal[i]);
                if !b.is_finite() {
                    &neutral
                } else if a > b {
                    &grow
                } else if a < b {
                    &fall
                } else {
                    &neutral
                }
            };
            let line_colors: Vec<Option<String>> = smi
                .iter()
                .enumerate()
                .map(|(i, x)| x.is_finite().then(|| ink_of(i).clone()))
                .collect();
            let band_colors: Vec<Option<String>> = smi
                .iter()
                .enumerate()
                .map(|(i, x)| x.is_finite().then(|| transp(ink_of(i), 75.0)))
                .collect();
            ClassicOutput {
                plots: vec![
                    plot(
                        "smi",
                        ("indicator.smiErgodic.plot.smi", "SMI"),
                        smi.clone(),
                        neutral.clone(),
                    )
                    .colors(line_colors),
                    plot(
                        "signal",
                        ("indicator.smiErgodic.plot.signal", "Signal"),
                        signal.clone(),
                        text(inputs, "signalColor", WARNING),
                    ),
                ],
                bands: vec![
                    ClassicBand::new("spread", "smi", "signal", transp(&neutral, 75.0))
                        .colors(band_colors),
                ],
                levels: zero,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.smiErgodic.shortTitle", "SMIE"))
}

const STC_MID: f64 = 50.0;
const TRIGGERS: &str = "Trigger Levels";

/// Position inside the window's own range; a flat window holds the previous reading.
fn stc_stoch_norm(v: &[f64], length: usize) -> Vec<f64> {
    let lo = lowest(v, length);
    let hi = highest(v, length);
    let mut out = vec![NAN; v.len()];
    let mut pct = NAN;
    for i in 0..v.len() {
        let (x, l, h) = (v[i], lo[i], hi[i]);
        if !x.is_finite() || !l.is_finite() || !h.is_finite() {
            continue;
        }
        if h > l {
            pct = (100.0 * (x - l)) / (h - l);
        }
        out[i] = pct;
    }
    out
}

/// `%D` style recursive smoothing: each reading moves `factor` of the way toward the input.
fn stc_smooth_step(v: &[f64], factor: f64) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    let mut smoothed = NAN;
    for (i, &x) in v.iter().enumerate() {
        if !x.is_finite() {
            continue;
        }
        smoothed = if smoothed.is_finite() {
            smoothed + factor * (x - smoothed)
        } else {
            x
        };
        out[i] = smoothed;
    }
    out
}

fn stc_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        srcin(
            "Close",
            "source",
            ("indicator.schaffTrendCycle.source", "Source"),
            (
                "indicator.schaffTrendCycle.source.tooltip",
                "Price series used to build the MACD that the trend cycle is measured on.",
            ),
        ),
        lenin(
            23.0,
            "fastLength",
            ("indicator.schaffTrendCycle.fastLength", "Fast Length"),
            5000.0,
            (
                "indicator.schaffTrendCycle.fastLength.tooltip",
                "Fast EMA length of the underlying MACD.",
            ),
        ),
        lenin(
            50.0,
            "slowLength",
            ("indicator.schaffTrendCycle.slowLength", "Slow Length"),
            5000.0,
            (
                "indicator.schaffTrendCycle.slowLength.tooltip",
                "Slow EMA length of the underlying MACD.",
            ),
        ),
        with_min(
            lenin(
                10.0,
                "cycleLength",
                ("indicator.schaffTrendCycle.cycleLength", "Cycle Length"),
                5000.0,
                (
                    "indicator.schaffTrendCycle.cycleLength.tooltip",
                    "Window of the two stochastic passes. Each pass measures where its input sits inside the range of its own values over this many bars.",
                ),
            ),
            2.0,
        ),
        fltin(
            "factor",
            ("indicator.schaffTrendCycle.factor", "Smoothing Factor"),
            0.5,
            0.01,
            1.0,
            0.05,
            (
                "indicator.schaffTrendCycle.factor.tooltip",
                "%D style recursive smoothing applied after each stochastic pass. 0.5 is the published default.",
            ),
        ),
        fltin(
            "upper",
            ("indicator.schaffTrendCycle.upper", "Upper Trigger"),
            75.0,
            0.0,
            100.0,
            1.0,
            (
                "indicator.schaffTrendCycle.upper.tooltip",
                "Upper trigger line. A cross down through it flags an emerging downtrend.",
            ),
        )
        .group(TRIGGERS),
        fltin(
            "lower",
            ("indicator.schaffTrendCycle.lower", "Lower Trigger"),
            25.0,
            0.0,
            100.0,
            1.0,
            (
                "indicator.schaffTrendCycle.lower.tooltip",
                "Lower trigger line. A cross up through it flags an emerging uptrend.",
            ),
        )
        .group(TRIGGERS),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.schaffTrendCycle.bullColor", "Bullish Color"),
            (
                "indicator.schaffTrendCycle.bullColor.tooltip",
                "Color of the trend cycle line and gradient while the cycle holds the upper half of its 0-100 scale.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.schaffTrendCycle.bearColor", "Bearish Color"),
            (
                "indicator.schaffTrendCycle.bearColor.tooltip",
                "Color of the trend cycle line and gradient while the cycle holds the lower half of its 0-100 scale.",
            ),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.schaffTrendCycle.levelsColor", "Levels Color"),
            (
                "indicator.schaffTrendCycle.levelsColor.tooltip",
                "Color of the dashed trigger lines and the dotted midline.",
            ),
        ),
        boolin(
            "gradientFill",
            ("indicator.schaffTrendCycle.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            (
                "indicator.schaffTrendCycle.gradientFill.tooltip",
                "Vertical gradient between the midline and the trend cycle, fading out toward the midline.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "schaff-trend-cycle",
        msg("indicator.schaffTrendCycle.title", "Schaff Trend Cycle"),
        false,
        inputs,
        |bars, inputs| {
            let cycle = len(inputs, "cycleLength", 10);
            let factor = num(inputs, "factor", 0.5);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let macd_line = sub(
                &ema(&src, len(inputs, "fastLength", 23)),
                &ema(&src, len(inputs, "slowLength", 50)),
            );
            let first = stc_smooth_step(&stc_stoch_norm(&macd_line, cycle), factor);
            let values = stc_smooth_step(&stc_stoch_norm(&first, cycle), factor);
            let level_ink = text(inputs, "levelsColor", NEUTRAL);
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let colors: Vec<Option<String>> = values
                .iter()
                .map(|&x| {
                    x.is_finite().then(|| {
                        if x > STC_MID {
                            bull.clone()
                        } else {
                            bear.clone()
                        }
                    })
                })
                .collect();
            let mut plots = vec![
                plot(
                    "stc",
                    ("indicator.schaffTrendCycle.plot.stc", "Schaff Trend Cycle"),
                    values.clone(),
                    bear.clone(),
                )
                .width(2.0)
                .colors(colors),
            ];
            let mut bands = Vec::new();
            if flag(inputs, "gradientFill", true) {
                plots.push(anchor_plot(
                    "midAnchor",
                    "Midline Anchor",
                    values.len(),
                    STC_MID,
                ));
                let wash = baseline_gradient(
                    &values,
                    STC_MID,
                    |x, _| {
                        if x > STC_MID {
                            bull.clone()
                        } else {
                            bear.clone()
                        }
                    },
                    VALUE_TRANSPARENCY,
                    BASE_TRANSPARENCY,
                );
                bands.push(
                    ClassicBand::new("wash", "stc", "midAnchor", transp(NEUTRAL, 100.0))
                        .gradient(wash),
                );
            }
            ClassicOutput {
                plots,
                bands,
                levels: vec![
                    level(
                        "upper",
                        num(inputs, "upper", 75.0),
                        transp(&level_ink, 25.0),
                        LineStyle::Dashed,
                        ("indicator.schaffTrendCycle.level.upper", "Upper Trigger"),
                    ),
                    level(
                        "lower",
                        num(inputs, "lower", 25.0),
                        transp(&level_ink, 25.0),
                        LineStyle::Dashed,
                        ("indicator.schaffTrendCycle.level.lower", "Lower Trigger"),
                    ),
                    level(
                        "midline",
                        STC_MID,
                        transp(&level_ink, 60.0),
                        LineStyle::Dotted,
                        ("indicator.schaffTrendCycle.level.midline", "Midline"),
                    ),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg(
        "indicator.schaffTrendCycle.shortTitle",
        "Schaff Trend Cycle",
    ))
}

fn kst_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            10.0,
            "rocLength1",
            ("indicator.knowSureThing.rocLength1", "ROC Length #1"),
            5000.0,
            (
                "indicator.knowSureThing.rocLength1.tooltip",
                "Lookback of the first (shortest) rate of change.",
            ),
        ),
        lenin(
            15.0,
            "rocLength2",
            ("indicator.knowSureThing.rocLength2", "ROC Length #2"),
            5000.0,
            (
                "indicator.knowSureThing.rocLength2.tooltip",
                "Lookback of the second rate of change.",
            ),
        ),
        lenin(
            20.0,
            "rocLength3",
            ("indicator.knowSureThing.rocLength3", "ROC Length #3"),
            5000.0,
            (
                "indicator.knowSureThing.rocLength3.tooltip",
                "Lookback of the third rate of change.",
            ),
        ),
        lenin(
            30.0,
            "rocLength4",
            ("indicator.knowSureThing.rocLength4", "ROC Length #4"),
            5000.0,
            (
                "indicator.knowSureThing.rocLength4.tooltip",
                "Lookback of the fourth (longest) rate of change.",
            ),
        ),
        lenin(
            10.0,
            "smaLength1",
            ("indicator.knowSureThing.smaLength1", "SMA Length #1"),
            5000.0,
            (
                "indicator.knowSureThing.smaLength1.tooltip",
                "Smoothing length applied to the first rate of change (RCMA #1).",
            ),
        ),
        lenin(
            10.0,
            "smaLength2",
            ("indicator.knowSureThing.smaLength2", "SMA Length #2"),
            5000.0,
            (
                "indicator.knowSureThing.smaLength2.tooltip",
                "Smoothing length applied to the second rate of change (RCMA #2).",
            ),
        ),
        lenin(
            10.0,
            "smaLength3",
            ("indicator.knowSureThing.smaLength3", "SMA Length #3"),
            5000.0,
            (
                "indicator.knowSureThing.smaLength3.tooltip",
                "Smoothing length applied to the third rate of change (RCMA #3).",
            ),
        ),
        lenin(
            15.0,
            "smaLength4",
            ("indicator.knowSureThing.smaLength4", "SMA Length #4"),
            5000.0,
            (
                "indicator.knowSureThing.smaLength4.tooltip",
                "Smoothing length applied to the fourth rate of change (RCMA #4).",
            ),
        ),
        lenin(
            9.0,
            "signalLength",
            ("indicator.knowSureThing.signalLength", "Signal Length"),
            500.0,
            (
                "indicator.knowSureThing.signalLength.tooltip",
                "Simple moving average length of the signal line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.knowSureThing.bullColor", "Bullish"),
            (
                "indicator.knowSureThing.bullColor.tooltip",
                "Color of the KST line while above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.knowSureThing.bearColor", "Bearish"),
            (
                "indicator.knowSureThing.bearColor.tooltip",
                "Color of the KST line while below zero.",
            ),
        ),
        clr(
            WARNING,
            "signalColor",
            ("indicator.knowSureThing.signalColor", "Signal"),
            (
                "indicator.knowSureThing.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "know-sure-thing",
        msg("indicator.knowSureThing.title", "Know Sure Thing"),
        false,
        inputs,
        |bars, inputs| {
            let c = closes(bars);
            let rcma = |roc_key: &str, roc_default: usize, sma_key: &str, sma_default: usize| {
                sma(
                    &roc(&c, len(inputs, roc_key, roc_default)),
                    len(inputs, sma_key, sma_default),
                )
            };
            let smoothed = [
                rcma("rocLength1", 10, "smaLength1", 10),
                rcma("rocLength2", 15, "smaLength2", 10),
                rcma("rocLength3", 20, "smaLength3", 10),
                rcma("rocLength4", 30, "smaLength4", 15),
            ];
            let values = smoothed
                .iter()
                .enumerate()
                .fold(vec![0.0; c.len()], |acc, (i, r)| {
                    zip(&acc, r, |a, b| a + b * (i + 1) as f64)
                });
            let bull = text(inputs, "bullColor", BULLISH);
            let signal = sma(&values, len(inputs, "signalLength", 9));
            let mut opts = CenteredOptions::new(
                "kst",
                msg("indicator.knowSureThing.plot.kst", "KST"),
                values,
                DirectionInk {
                    bull: bull.clone(),
                    bear: text(inputs, "bearColor", BEARISH),
                    neutral: bull,
                },
                NEUTRAL,
            );
            opts.value_transparency = Some(60.0);
            opts.extra_plots = vec![plot(
                "signal",
                ("indicator.knowSureThing.plot.signal", "Signal"),
                signal,
                text(inputs, "signalColor", WARNING),
            )];
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.knowSureThing.shortTitle", "KST"))
}

fn coppock_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            10.0,
            "wmaLength",
            ("indicator.coppockCurve.wmaLength", "WMA Length"),
            5000.0,
            (
                "indicator.coppockCurve.wmaLength.tooltip",
                "Length of the weighted moving average applied to the sum of the two rates of change.",
            ),
        ),
        lenin(
            14.0,
            "longRoc",
            ("indicator.coppockCurve.longRoc", "Long ROC Length"),
            5000.0,
            (
                "indicator.coppockCurve.longRoc.tooltip",
                "Lookback of the long rate of change, in bars. The original definition uses 14 monthly closes.",
            ),
        ),
        lenin(
            11.0,
            "shortRoc",
            ("indicator.coppockCurve.shortRoc", "Short ROC Length"),
            5000.0,
            (
                "indicator.coppockCurve.shortRoc.tooltip",
                "Lookback of the short rate of change, in bars. The original definition uses 11 monthly closes.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.coppockCurve.source", "Source"),
            (
                "indicator.coppockCurve.source.tooltip",
                "Price series used for the rate of change calculations.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.coppockCurve.color", "Coppock Curve"),
            (
                "indicator.coppockCurve.color.tooltip",
                "Color of the Coppock Curve line.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.coppockCurve.bullColor", "Bullish"),
            (
                "indicator.coppockCurve.bullColor.tooltip",
                "Line and fill color while the curve reads above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.coppockCurve.bearColor", "Bearish"),
            (
                "indicator.coppockCurve.bearColor.tooltip",
                "Line and fill color while the curve reads below zero.",
            ),
        ),
        clr(
            NEUTRAL,
            "zeroColor",
            ("indicator.coppockCurve.zeroColor", "Zero Line"),
            (
                "indicator.coppockCurve.zeroColor.tooltip",
                "Color of the zero reference line.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "coppock-curve",
        msg("indicator.coppockCurve.title", "Coppock Curve"),
        false,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let both = zip(
                &roc(&src, len(inputs, "longRoc", 14)),
                &roc(&src, len(inputs, "shortRoc", 11)),
                |a, b| a + b,
            );
            let values = wma(&both, len(inputs, "wmaLength", 10));
            let opts = CenteredOptions::new(
                "coppock",
                msg("indicator.coppockCurve.plot.coppock", "Coppock Curve"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                text(inputs, "zeroColor", NEUTRAL),
            );
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.coppockCurve.shortTitle", "Coppock"))
}

fn dpo_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            21.0,
            "length",
            ("indicator.detrendedPriceOscillator.length", "Length"),
            5000.0,
            (
                "indicator.detrendedPriceOscillator.length.tooltip",
                "Lookback of the simple moving average that price is detrended against. The comparison is displaced by Length / 2 + 1 bars.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.detrendedPriceOscillator.source", "Source"),
            (
                "indicator.detrendedPriceOscillator.source.tooltip",
                "Price series used in the detrending calculation.",
            ),
        ),
        boolin(
            "centered",
            ("indicator.detrendedPriceOscillator.centered", "Centered"),
            false,
            SETTINGS,
            (
                "indicator.detrendedPriceOscillator.centered.tooltip",
                "Shifts the oscillator back by Length / 2 + 1 bars so it is centered on the prices it detrends. The most recent bars then show no value.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.detrendedPriceOscillator.color", "DPO"),
            (
                "indicator.detrendedPriceOscillator.color.tooltip",
                "Color of the line when it sits exactly at zero.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.detrendedPriceOscillator.bullColor", "Above Zero"),
            (
                "indicator.detrendedPriceOscillator.bullColor.tooltip",
                "Color of the line and fill while the oscillator is above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.detrendedPriceOscillator.bearColor", "Below Zero"),
            (
                "indicator.detrendedPriceOscillator.bearColor.tooltip",
                "Color of the line and fill while the oscillator is below zero.",
            ),
        ),
        clr(
            NEUTRAL,
            "zeroColor",
            ("indicator.detrendedPriceOscillator.zeroColor", "Zero Line"),
            (
                "indicator.detrendedPriceOscillator.zeroColor.tooltip",
                "Color of the zero reference line.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "detrended-price-oscillator",
        msg(
            "indicator.detrendedPriceOscillator.title",
            "Detrended Price Oscillator",
        ),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 21);
            let bars_back = (length / 2 + 1) as i64;
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let basis = sma(&src, length);
            // Centered mode compares the displaced price with today's average, then plots the
            // result back over the bars it detrends, leaving the newest bars empty.
            let values = if flag(inputs, "centered", false) {
                shift(&sub(&shift(&src, bars_back), &basis), -bars_back)
            } else {
                sub(&src, &shift(&basis, bars_back))
            };
            let opts = CenteredOptions::new(
                "dpo",
                msg("indicator.detrendedPriceOscillator.plot.dpo", "DPO"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                text(inputs, "zeroColor", NEUTRAL),
            );
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.detrendedPriceOscillator.shortTitle", "DPO"))
}

const ROC_PERCENT: &str = "ROC %";
const ROC_MOMENTUM: &str = "Momentum";

fn roc_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            9.0,
            "length",
            ("indicator.rateOfChange.length", "Length"),
            5000.0,
            (
                "indicator.rateOfChange.length.tooltip",
                "Lookback period; the source is compared with its value this many bars ago.",
            ),
        ),
        srcin(
            "Close",
            "source",
            ("indicator.rateOfChange.source", "Source"),
            (
                "indicator.rateOfChange.source.tooltip",
                "Series used as input for the calculation.",
            ),
        ),
        optin(
            "mode",
            ("indicator.rateOfChange.mode", "Mode"),
            ROC_PERCENT,
            &[ROC_PERCENT, ROC_MOMENTUM],
            (
                "indicator.rateOfChange.mode.tooltip",
                "ROC % plots the change as a percentage of the source value from the lookback; Momentum plots the raw difference.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.rateOfChange.color", "Line"),
            (
                "indicator.rateOfChange.color.tooltip",
                "Color of the line when the reading sits exactly at zero.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.rateOfChange.bullColor", "Bullish"),
            (
                "indicator.rateOfChange.bullColor.tooltip",
                "Color of the line and fill while the reading is above zero.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.rateOfChange.bearColor", "Bearish"),
            (
                "indicator.rateOfChange.bearColor.tooltip",
                "Color of the line and fill while the reading is below zero.",
            ),
        ),
        boolin(
            "gradientFill",
            ("indicator.rateOfChange.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            (
                "indicator.rateOfChange.gradientFill.tooltip",
                "Fills the area between the line and zero, following the line color.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "rate-of-change",
        msg("indicator.rateOfChange.title", "Rate of Change"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 9);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let values = if text(inputs, "mode", ROC_PERCENT) == ROC_MOMENTUM {
                change(&src, length)
            } else {
                roc(&src, length)
            };
            let mut opts = CenteredOptions::new(
                "roc",
                msg("indicator.rateOfChange.plot.roc", "ROC"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                NEUTRAL,
            );
            opts.fill = flag(inputs, "gradientFill", true);
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.rateOfChange.shortTitle", "ROC"))
}

fn ultimate_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            7.0,
            "fastLength",
            ("indicator.ultimateOscillator.fastLength", "Fast Length"),
            5000.0,
            (
                "indicator.ultimateOscillator.fastLength.tooltip",
                "Bars in the fastest averaging window. Weighted 4 in the final blend, so it dominates the oscillator.",
            ),
        ),
        lenin(
            14.0,
            "middleLength",
            ("indicator.ultimateOscillator.middleLength", "Middle Length"),
            5000.0,
            (
                "indicator.ultimateOscillator.middleLength.tooltip",
                "Bars in the intermediate averaging window. Weighted 2 in the final blend.",
            ),
        ),
        lenin(
            28.0,
            "slowLength",
            ("indicator.ultimateOscillator.slowLength", "Slow Length"),
            5000.0,
            (
                "indicator.ultimateOscillator.slowLength.tooltip",
                "Bars in the slowest averaging window. Weighted 1 in the final blend.",
            ),
        ),
    ];
    inputs.extend(threshold_inputs(
        70.0,
        30.0,
        0.0,
        100.0,
        Some((
            ("indicator.ultimateOscillator.overbought", "Overbought"),
            ("indicator.ultimateOscillator.oversold", "Oversold"),
        )),
    ));
    inputs.extend([
        clr(
            NEUTRAL,
            "color",
            ("indicator.ultimateOscillator.color", "Oscillator"),
            ("indicator.ultimateOscillator.color.tooltip", "Color of the line midway between the levels. Toward either level it blends into that level’s color."),
        ),
        clr(
            NEUTRAL,
            "levelsColor",
            ("indicator.ultimateOscillator.levelsColor", "Levels"),
            ("indicator.ultimateOscillator.levelsColor.tooltip", "Color of the overbought and oversold levels."),
        ),
        clr(
            BULLISH,
            "oversoldColor",
            ("indicator.ultimateOscillator.oversoldColor", "Oversold"),
            ("indicator.ultimateOscillator.oversoldColor.tooltip", "Color of the line at the oversold level and of the oversold zone shading."),
        ),
        clr(
            BEARISH,
            "overboughtColor",
            ("indicator.ultimateOscillator.overboughtColor", "Overbought"),
            ("indicator.ultimateOscillator.overboughtColor.tooltip", "Color of the line at the overbought level and of the overbought zone shading."),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "ultimate-oscillator",
        msg("indicator.ultimateOscillator.title", "Ultimate Oscillator"),
        false,
        inputs,
        |bars, inputs| {
            let n = bars.len();
            let mut bp = vec![NAN; n];
            let mut tr = vec![NAN; n];
            for (i, b) in bars.iter().enumerate() {
                let pc = if i > 0 { bars[i - 1].close } else { b.close };
                let true_low = js_min(b.low, pc);
                bp[i] = b.close - true_low;
                tr[i] = js_max(b.high, pc) - true_low;
            }
            let avg = |length: usize| {
                zip(&sum(&bp, length), &sum(&tr, length), |a, b| {
                    if b == 0.0 { NAN } else { a / b }
                })
            };
            let a7 = avg(len(inputs, "fastLength", 7));
            let a14 = avg(len(inputs, "middleLength", 14));
            let a28 = avg(len(inputs, "slowLength", 28));
            let values: Vec<f64> = (0..n)
                .map(|i| {
                    let (x, y, z) = (a7[i], a14[i], a28[i]);
                    if x.is_finite() && y.is_finite() && z.is_finite() {
                        (100.0 * (4.0 * x + 2.0 * y + z)) / 7.0
                    } else {
                        NAN
                    }
                })
                .collect();
            let mut opts = bounded(
                "uo",
                ("indicator.ultimateOscillator.plot.uo", "UO"),
                values,
                (
                    num(inputs, "overbought", 70.0),
                    num(inputs, "oversold", 30.0),
                ),
                (100.0, 0.0),
                ramp_ink(
                    inputs,
                    ("oversoldColor", BULLISH),
                    NEUTRAL,
                    ("overboughtColor", BEARISH),
                ),
                text(inputs, "levelsColor", NEUTRAL),
            );
            opts.zone_transparency = Some(91.0);
            from_parts(bounded_oscillator(opts))
        },
    )
    .short_title(msg("indicator.ultimateOscillator.shortTitle", "UO"))
}

fn rvgi_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            10.0,
            "length",
            ("indicator.relativeVigorIndex.length", "Length"),
            5000.0,
            (
                "indicator.relativeVigorIndex.length.tooltip",
                "Number of bars summed for the smoothed numerator (close − open) and denominator (high − low).",
            ),
        ),
        clr(
            BULLISH,
            "color",
            ("indicator.relativeVigorIndex.color", "RVGI"),
            (
                "indicator.relativeVigorIndex.color.tooltip",
                "Color of the RVGI line.",
            ),
        ),
        clr(
            BEARISH,
            "signalColor",
            ("indicator.relativeVigorIndex.signalColor", "Signal"),
            (
                "indicator.relativeVigorIndex.signalColor.tooltip",
                "Color of the signal line.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "relative-vigor-index",
        msg("indicator.relativeVigorIndex.title", "Relative Vigor Index"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 10);
            let co = swma(&sub(&closes(bars), &opens(bars)));
            let hl = swma(&sub(&highs(bars), &lows(bars)));
            let line = zip(&sum(&co, length), &sum(&hl, length), |a, b| {
                if b == 0.0 { 0.0 } else { a / b }
            });
            let signal = swma(&line);
            ClassicOutput {
                plots: vec![
                    plot(
                        "rvgi",
                        ("indicator.relativeVigorIndex.plot.rvgi", "RVGI"),
                        line,
                        text(inputs, "color", BULLISH),
                    ),
                    plot(
                        "signal",
                        ("indicator.relativeVigorIndex.plot.signal", "Signal"),
                        signal,
                        text(inputs, "signalColor", BEARISH),
                    ),
                ],
                levels: vec![level(
                    "zero",
                    0.0,
                    transp(NEUTRAL, 50.0),
                    LineStyle::Dashed,
                    ("indicator.relativeVigorIndex.level.zero", "Zero Line"),
                )],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.relativeVigorIndex.shortTitle", "RVGI"))
}

const RVI_EMA: &str = "EMA";
const RVI_RMA: &str = "Wilder (RMA)";

fn relative_volatility_spec() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        lenin(
            10.0,
            "stdevLength",
            (
                "indicator.relativeVolatilityIndex.stdevLength",
                "Stdev Length",
            ),
            5000.0,
            (
                "indicator.relativeVolatilityIndex.stdevLength.tooltip",
                "Number of bars used for the standard deviation of closing prices that serves as each bar's volatility reading.",
            ),
        ),
        lenin(
            14.0,
            "smoothLength",
            (
                "indicator.relativeVolatilityIndex.smoothLength",
                "Smoothing Length",
            ),
            5000.0,
            (
                "indicator.relativeVolatilityIndex.smoothLength.tooltip",
                "Number of bars used to smooth the up and down volatility streams.",
            ),
        ),
        optin(
            "smoothType",
            (
                "indicator.relativeVolatilityIndex.smoothType",
                "Smoothing Type",
            ),
            RVI_EMA,
            &[RVI_EMA, RVI_RMA],
            (
                "indicator.relativeVolatilityIndex.smoothType.tooltip",
                "Moving average applied to the volatility streams. EMA is Dorsey's classic calculation; Wilder (RMA) matches platforms that use Wilder smoothing.",
            ),
        ),
    ];
    inputs.extend(thresholds(80.0, 20.0));
    inputs.extend([
        clr(
            NEUTRAL,
            "color",
            ("indicator.relativeVolatilityIndex.color", "RVI Line"),
            ("indicator.relativeVolatilityIndex.color.tooltip", "Color of the RVI line at the midline. Toward either threshold it blends into that threshold’s color."),
        ),
        clr(
            BULLISH,
            "overboughtColor",
            ("indicator.relativeVolatilityIndex.overboughtColor", "Overbought"),
            ("indicator.relativeVolatilityIndex.overboughtColor.tooltip", "Color of the line and zone shading at and above the overbought level."),
        ),
        clr(
            BEARISH,
            "oversoldColor",
            ("indicator.relativeVolatilityIndex.oversoldColor", "Oversold"),
            ("indicator.relativeVolatilityIndex.oversoldColor.tooltip", "Color of the line and zone shading at and below the oversold level."),
        ),
    ]);
    ClassicIndicatorSpec::new(
        "relative-volatility-index",
        msg(
            "indicator.relativeVolatilityIndex.title",
            "Relative Volatility Index",
        ),
        false,
        inputs,
        |bars, inputs| {
            let c = closes(bars);
            let sigma = stdev(&c, len(inputs, "stdevLength", 10));
            let d = change(&c, 1);
            let up = zip(&sigma, &d, |s, x| if x > 0.0 { s } else { 0.0 });
            let down = zip(&sigma, &d, |s, x| if x < 0.0 { s } else { 0.0 });
            let smooth_len = len(inputs, "smoothLength", 14);
            let smooth: fn(&[f64], usize) -> Vec<f64> =
                if text(inputs, "smoothType", RVI_EMA) == RVI_RMA {
                    rma
                } else {
                    ema
                };
            let values = zip(
                &smooth(&up, smooth_len),
                &smooth(&down, smooth_len),
                |u, w| {
                    if u + w == 0.0 {
                        0.0
                    } else {
                        (100.0 * u) / (u + w)
                    }
                },
            );
            let mut opts = bounded(
                "rvi",
                ("indicator.relativeVolatilityIndex.plot.rvi", "RVI"),
                values,
                (
                    num(inputs, "overbought", 80.0),
                    num(inputs, "oversold", 20.0),
                ),
                (100.0, 0.0),
                ramp_ink(
                    inputs,
                    ("oversoldColor", BEARISH),
                    NEUTRAL,
                    ("overboughtColor", BULLISH),
                ),
                NEUTRAL.to_string(),
            );
            opts.width = Some(2.0);
            from_parts(bounded_oscillator(opts))
        },
    )
    .short_title(msg("indicator.relativeVolatilityIndex.shortTitle", "RVI"))
}

fn bop_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        boolin(
            "smoothing",
            ("indicator.balanceOfPower.smoothing", "Smoothing"),
            false,
            SETTINGS,
            (
                "indicator.balanceOfPower.smoothing.tooltip",
                "Smooth the raw Balance of Power with a simple moving average instead of plotting the single-bar value.",
            ),
        ),
        lenin(
            14.0,
            "smoothLength",
            ("indicator.balanceOfPower.smoothLength", "Length"),
            5000.0,
            (
                "indicator.balanceOfPower.smoothLength.tooltip",
                "Simple moving average length applied when smoothing is enabled.",
            ),
        ),
        clr(
            NEUTRAL,
            "color",
            ("indicator.balanceOfPower.color", "BOP Color"),
            (
                "indicator.balanceOfPower.color.tooltip",
                "Color of the line at exactly zero.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.balanceOfPower.bullColor", "Bullish"),
            (
                "indicator.balanceOfPower.bullColor.tooltip",
                "Line and fill color while the Balance of Power is above zero — buyers in control of the bar.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.balanceOfPower.bearColor", "Bearish"),
            (
                "indicator.balanceOfPower.bearColor.tooltip",
                "Line and fill color while the Balance of Power is below zero — sellers in control of the bar.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "balance-of-power",
        msg("indicator.balanceOfPower.title", "Balance of Power"),
        false,
        inputs,
        |bars, inputs| {
            let raw: Vec<f64> = bars
                .iter()
                .map(|b| {
                    if b.high == b.low {
                        0.0
                    } else {
                        (b.close - b.open) / (b.high - b.low)
                    }
                })
                .collect();
            let values = if flag(inputs, "smoothing", false) {
                sma(&raw, len(inputs, "smoothLength", 14))
            } else {
                raw
            };
            let mut opts = CenteredOptions::new(
                "bop",
                msg("indicator.balanceOfPower.plot.bop", "BOP"),
                values,
                direction_ink(inputs, text(inputs, "color", NEUTRAL)),
                NEUTRAL,
            );
            opts.value_transparency = Some(60.0);
            from_parts(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.balanceOfPower.shortTitle", "BOP"))
}

fn elder_ray_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        lenin(
            13.0,
            "length",
            ("indicator.elderRay.length", "EMA Length"),
            5000.0,
            (
                "indicator.elderRay.length.tooltip",
                "Number of bars in the exponential moving average of closing prices used as the reference level for Bull Power and Bear Power. Alexander Elder's classic setting is 13.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.elderRay.bullColor", "Bull Power"),
            (
                "indicator.elderRay.bullColor.tooltip",
                "Color of the Bull Power columns.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.elderRay.bearColor", "Bear Power"),
            (
                "indicator.elderRay.bearColor.tooltip",
                "Color of the Bear Power columns.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "elder-ray",
        msg("indicator.elderRay.title", "Elder Ray"),
        false,
        inputs,
        |bars, inputs| {
            let basis = ema(&closes(bars), len(inputs, "length", 13));
            ClassicOutput {
                plots: vec![
                    plot(
                        "bull",
                        ("indicator.elderRay.plot.bull", "Bull Power"),
                        sub(&highs(bars), &basis),
                        text(inputs, "bullColor", BULLISH),
                    )
                    .kind(LineLikeKind::Columns)
                    .base(0.0),
                    plot(
                        "bear",
                        ("indicator.elderRay.plot.bear", "Bear Power"),
                        sub(&lows(bars), &basis),
                        text(inputs, "bearColor", BEARISH),
                    )
                    .kind(LineLikeKind::Columns)
                    .base(0.0),
                ],
                levels: vec![level(
                    "zero",
                    0.0,
                    transp(NEUTRAL, 30.0),
                    LineStyle::Dashed,
                    ("indicator.elderRay.level.zero", "Zero Line"),
                )],
                ..Default::default()
            }
        },
    )
}

fn ttm_squeeze_spec() -> ClassicIndicatorSpec {
    let inputs = vec![
        with_min(
            lenin(
                20.0,
                "length",
                ("indicator.ttmSqueeze.length", "Length"),
                5000.0,
                (
                    "indicator.ttmSqueeze.length.tooltip",
                    "Shared lookback for the Bollinger Bands, the Keltner Channels, and the momentum regression. John Carter's original setting is 20.",
                ),
            ),
            2.0,
        ),
        srcin(
            "Close",
            "source",
            ("indicator.ttmSqueeze.source", "Source"),
            (
                "indicator.ttmSqueeze.source.tooltip",
                "Price series used for the shared moving average basis, the standard deviation, and the momentum displacement.",
            ),
        ),
        fltin(
            "bbMult",
            ("indicator.ttmSqueeze.bbMult", "Bollinger Bands Multiplier"),
            2.0,
            0.1,
            50.0,
            0.1,
            (
                "indicator.ttmSqueeze.bbMult.tooltip",
                "Standard deviation multiplier for the Bollinger Bands.",
            ),
        ),
        fltin(
            "kcMult",
            ("indicator.ttmSqueeze.kcMult", "Keltner Channels Multiplier"),
            1.5,
            0.1,
            50.0,
            0.1,
            (
                "indicator.ttmSqueeze.kcMult.tooltip",
                "ATR multiplier for the Keltner Channels. A smaller multiplier demands a tighter compression before the squeeze turns on.",
            ),
        ),
        clr(
            BULLISH,
            "bullColor",
            ("indicator.ttmSqueeze.bullColor", "Momentum Up"),
            (
                "indicator.ttmSqueeze.bullColor.tooltip",
                "Histogram color while momentum is positive. Bars building away from zero print solid, bars fading back print dimmed.",
            ),
        ),
        clr(
            BEARISH,
            "bearColor",
            ("indicator.ttmSqueeze.bearColor", "Momentum Down"),
            (
                "indicator.ttmSqueeze.bearColor.tooltip",
                "Histogram color while momentum is negative. Bars building away from zero print solid, bars fading back print dimmed.",
            ),
        ),
        clr(
            WARNING,
            "squeezeOnColor",
            ("indicator.ttmSqueeze.squeezeOnColor", "Squeeze On"),
            (
                "indicator.ttmSqueeze.squeezeOnColor.tooltip",
                "Zero-line dot color while both Bollinger Bands sit fully inside the Keltner Channels.",
            ),
        ),
        clr(
            NEUTRAL,
            "squeezeOffColor",
            ("indicator.ttmSqueeze.squeezeOffColor", "Squeeze Off"),
            (
                "indicator.ttmSqueeze.squeezeOffColor.tooltip",
                "Zero-line dot color while the Bollinger Bands trade outside the Keltner Channels.",
            ),
        ),
        boolin(
            "markFires",
            ("indicator.ttmSqueeze.markFires", "Mark Squeeze Fires"),
            true,
            STYLE,
            (
                "indicator.ttmSqueeze.markFires.tooltip",
                "Draws a cross on the zero line on the first bar where the bands re-emerge outside the channels, colored by the side of the momentum histogram.",
            ),
        ),
    ];
    ClassicIndicatorSpec::new(
        "ttm-squeeze",
        msg("indicator.ttmSqueeze.title", "TTM Squeeze"),
        false,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 20);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let basis = sma(&src, length);
            let bb_mult = num(inputs, "bbMult", 2.0);
            let kc_mult = num(inputs, "kcMult", 1.5);
            let dev = map(&stdev(&src, length), |x| x * bb_mult);
            let kc_range = map(&atr(bars, length), |x| x * kc_mult);
            // Momentum: regression of price against an anchor midway between the range
            // midpoint and the shared basis.
            let range_mid = zip(
                &highest(&highs(bars), length),
                &lowest(&lows(bars), length),
                |h, l| (h + l) / 2.0,
            );
            let anchor = zip(&range_mid, &basis, |d, b| (d + b) / 2.0);
            let mom = linreg(&sub(&src, &anchor), length, 0.0);
            let squeeze_on: Vec<Option<bool>> = basis
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    let (d, r) = (dev[i], kc_range[i]);
                    (b.is_finite() && d.is_finite() && r.is_finite()).then_some(d < r)
                })
                .collect();
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let on_ink = text(inputs, "squeezeOnColor", WARNING);
            let off_ink = text(inputs, "squeezeOffColor", NEUTRAL);
            let mom_colors: Vec<Option<String>> = mom
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    if !x.is_finite() {
                        return None;
                    }
                    let prev = if i > 0 && mom[i - 1].is_finite() {
                        mom[i - 1]
                    } else {
                        x
                    };
                    let rising = x > prev;
                    Some(if x >= 0.0 {
                        if rising {
                            bull.clone()
                        } else {
                            transp(&bull, 60.0)
                        }
                    } else if rising {
                        transp(&bear, 60.0)
                    } else {
                        bear.clone()
                    })
                })
                .collect();
            let state_colors: Vec<Option<String>> = squeeze_on
                .iter()
                .map(|s| s.map(|on| if on { on_ink.clone() } else { off_ink.clone() }))
                .collect();
            let mut plots = vec![
                columns(
                    "mom",
                    ("indicator.ttmSqueeze.plot.mom", "Momentum"),
                    mom.clone(),
                    bull.clone(),
                    mom_colors,
                ),
                plot(
                    "state",
                    ("indicator.ttmSqueeze.plot.state", "Squeeze State"),
                    squeeze_on
                        .iter()
                        .map(|s| if s.is_none() { NAN } else { 0.0 })
                        .collect(),
                    off_ink.clone(),
                )
                .kind(LineLikeKind::Circles)
                .width(2.0)
                .colors(state_colors),
            ];
            if flag(inputs, "markFires", true) {
                // The fire bar: the first bar the bands re-emerge outside the channels.
                let fires: Vec<f64> = (0..squeeze_on.len())
                    .map(|i| {
                        if squeeze_on[i] == Some(false) && i > 0 && squeeze_on[i - 1] == Some(true)
                        {
                            0.0
                        } else {
                            NAN
                        }
                    })
                    .collect();
                let fire_colors: Vec<Option<String>> = fires
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        x.is_finite().then(|| {
                            if mom[i].is_finite() {
                                if mom[i] >= 0.0 {
                                    bull.clone()
                                } else {
                                    bear.clone()
                                }
                            } else {
                                off_ink.clone()
                            }
                        })
                    })
                    .collect();
                plots.push(
                    plot(
                        "fire",
                        ("indicator.ttmSqueeze.plot.fire", "Squeeze Fire"),
                        fires,
                        off_ink,
                    )
                    .kind(LineLikeKind::Cross)
                    .width(3.0)
                    .colors(fire_colors),
                );
            }
            ClassicOutput {
                plots,
                ..Default::default()
            }
        },
    )
}

/// Every momentum oscillator, in the original's order.
pub fn oscillator_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        rsi_spec(),
        stochastic_spec(),
        stochastic_rsi_spec(),
        macd_spec(),
        ppo_spec(),
        awesome_spec(),
        cci_spec(),
        williams_r_spec(),
        cmo_spec(),
        connors_rsi_spec(),
        fisher_spec(),
        trix_spec(),
        tsi_spec(),
        smi_ergodic_spec(),
        stc_spec(),
        kst_spec(),
        coppock_spec(),
        dpo_spec(),
        roc_spec(),
        ultimate_spec(),
        rvgi_spec(),
        relative_volatility_spec(),
        bop_spec(),
        elder_ray_spec(),
        ttm_squeeze_spec(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::source_text;
    use crate::indicators::classics::define::classic_native_output;
    use crate::model::{InputValue, Ohlcv, SeriesData};

    fn bar(close: f64, i: usize, volume: Option<f64>) -> Ohlcv {
        Ohlcv::new(
            i as i64 * 60_000,
            close - 0.3,
            close + 1.0,
            close - 1.0,
            close,
            volume,
        )
    }

    fn synthetic(n: usize) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| {
                bar(
                    100.0
                        + (i as f64 / 7.0).sin() * 8.0
                        + (i as f64 / 23.0).cos() * 5.0
                        + i as f64 * 0.02,
                    i,
                    Some(1000.0 + (i % 13) as f64 * 50.0),
                )
            })
            .collect()
    }

    fn spec(kind: &str) -> ClassicIndicatorSpec {
        oscillator_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    fn values_of(out: &ClassicOutput, key: &str) -> Vec<f64> {
        out.plots
            .iter()
            .find(|p| p.key == key)
            .expect("plot exists")
            .values
            .clone()
    }

    fn same(a: f64, b: f64) -> bool {
        (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-9
    }

    #[test]
    fn catalog_lists_every_oscillator_in_order() {
        let types: Vec<&str> = oscillator_specs()
            .iter()
            .map(|s| s.indicator_type)
            .collect();
        assert_eq!(
            types,
            vec![
                "rsi",
                "stochastic",
                "stochastic-rsi",
                "macd",
                "ppo",
                "awesome-oscillator",
                "commodity-channel-index",
                "williams-percent-r",
                "chande-momentum-oscillator",
                "connors-rsi",
                "fisher-transform",
                "trix",
                "true-strength-index",
                "smi-ergodic",
                "schaff-trend-cycle",
                "know-sure-thing",
                "coppock-curve",
                "detrended-price-oscillator",
                "rate-of-change",
                "ultimate-oscillator",
                "relative-vigor-index",
                "relative-volatility-index",
                "balance-of-power",
                "elder-ray",
                "ttm-squeeze",
            ]
        );
    }

    #[test]
    fn every_spec_computes_bar_aligned_plots_with_a_visible_one() {
        let bars = synthetic(400);
        for spec in oscillator_specs() {
            let out = spec.compute(&bars, &spec.default_inputs());
            assert!(!out.plots.is_empty(), "{}", spec.indicator_type);
            for p in &out.plots {
                assert_eq!(
                    p.values.len(),
                    bars.len(),
                    "{} / {}",
                    spec.indicator_type,
                    p.key
                );
                if let Some(colors) = &p.colors {
                    assert_eq!(
                        colors.len(),
                        bars.len(),
                        "{} / {} colors",
                        spec.indicator_type,
                        p.key
                    );
                }
            }
            let visible = out
                .plots
                .iter()
                .any(|p| !p.overlay && p.display.is_none_or(|d| d.pane != Some(false)));
            assert!(visible, "{} keeps a visible plot", spec.indicator_type);
            assert!(
                out.plots
                    .iter()
                    .any(|p| p.values.iter().any(|v| v.is_finite())),
                "{} produces values",
                spec.indicator_type
            );
        }
    }

    #[test]
    fn every_band_references_existing_plots() {
        let bars = synthetic(120);
        for spec in oscillator_specs() {
            let out = spec.compute(&bars, &spec.default_inputs());
            for band in &out.bands {
                assert!(
                    out.plots.iter().any(|p| p.key == band.from),
                    "{} band {} from",
                    spec.indicator_type,
                    band.key
                );
                assert!(
                    out.plots.iter().any(|p| p.key == band.to),
                    "{} band {} to",
                    spec.indicator_type,
                    band.key
                );
            }
        }
    }

    #[test]
    fn short_titles_match_the_library_declarations() {
        let expected = [
            ("awesome-oscillator", "AO"),
            ("balance-of-power", "BOP"),
            ("chande-momentum-oscillator", "CMO"),
            ("commodity-channel-index", "CCI"),
            ("connors-rsi", "CRSI"),
            ("coppock-curve", "Coppock"),
            ("detrended-price-oscillator", "DPO"),
            ("elder-ray", "Elder Ray"),
            ("fisher-transform", "Fisher"),
            ("know-sure-thing", "KST"),
            ("macd", "MACD"),
            ("ppo", "PPO"),
            ("rate-of-change", "ROC"),
            ("rsi", "RSI"),
            ("relative-vigor-index", "RVGI"),
            ("relative-volatility-index", "RVI"),
            ("schaff-trend-cycle", "Schaff Trend Cycle"),
            ("smi-ergodic", "SMIE"),
            ("stochastic", "Stoch"),
            ("stochastic-rsi", "Stoch RSI"),
            ("trix", "TRIX"),
            ("true-strength-index", "TSI"),
            ("ttm-squeeze", "TTM Squeeze"),
            ("ultimate-oscillator", "UO"),
            ("williams-percent-r", "%R"),
        ];
        for (kind, short) in expected {
            let s = spec(kind);
            assert_eq!(
                source_text(s.short_title.as_ref().unwrap_or(&s.title)),
                short,
                "{kind}"
            );
        }
    }

    #[test]
    fn rate_of_change_declares_its_two_modes() {
        let s = spec("rate-of-change");
        let mode = s
            .inputs
            .iter()
            .find(|i| i.key == "mode")
            .expect("mode input");
        assert_eq!(
            mode.options.as_deref(),
            Some(&["ROC %".to_string(), "Momentum".to_string()][..])
        );
    }

    #[test]
    fn rate_of_change_momentum_mode_is_the_raw_difference() {
        let bars = synthetic(60);
        let s = spec("rate-of-change");
        let mut inputs = s.default_inputs();
        inputs.insert("mode".into(), InputValue::from("Momentum"));
        let out = s.compute(&bars, &inputs);
        let roc_line = values_of(&out, "roc");
        assert!(same(roc_line[30], bars[30].close - bars[21].close));
    }

    #[test]
    fn rsi_reaches_100_on_a_monotonic_rise() {
        let rising: Vec<Ohlcv> = (0..50).map(|i| bar(100.0 + i as f64, i, None)).collect();
        let s = spec("rsi");
        let out = s.compute(&rising, &s.default_inputs());
        assert!((out.plots[0].values[49] - 100.0).abs() < 1e-4);
        assert_eq!(out.plots[0].key, "rsi");
        let keys: Vec<&str> = out.levels.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, vec!["overbought", "midline", "oversold"]);
    }

    #[test]
    fn macd_line_is_the_fast_ema_minus_the_slow_ema() {
        let bars = synthetic(200);
        let s = spec("macd");
        let out = s.compute(&bars, &s.default_inputs());
        let c = closes(&bars);
        let expected = sub(&ema(&c, 12), &ema(&c, 26));
        let line = values_of(&out, "macd");
        assert!(line.iter().zip(&expected).all(|(a, b)| same(*a, *b)));
        let signal = values_of(&out, "signal");
        let hist = values_of(&out, "hist");
        let i = 100;
        assert!(same(hist[i], line[i] - signal[i]));
        assert_eq!(out.plots[0].key, "hist");
        assert_eq!(out.plots[0].kind, LineLikeKind::Columns);
    }

    #[test]
    fn macd_series_ids_are_stable_across_recomputes() {
        let s = spec("macd");
        let mut bars: Vec<Ohlcv> = (0..100)
            .map(|i| bar(100.0 + (i as f64 / 5.0).sin(), i, Some(1000.0)))
            .collect();
        let first = classic_native_output(&s, &bars, &s.default_inputs());
        bars.push(bar(101.0, 100, Some(1000.0)));
        let second = classic_native_output(&s, &bars, &s.default_inputs());
        let ids = |o: &crate::indicators::NativeIndicatorOutput| {
            o.series.iter().map(|x| x.id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second));
        assert_eq!(first.price_lines.len(), 1);
        assert_eq!(first.price_lines[0].price, 0.0);
        match &second.series[0].data {
            SeriesData::LineLike { points, .. } => assert_eq!(points.len(), 101),
            other => panic!("unexpected series data: {other:?}"),
        }
    }

    #[test]
    fn stochastic_stays_inside_its_scale() {
        let bars = synthetic(300);
        let s = spec("stochastic");
        let out = s.compute(&bars, &s.default_inputs());
        for v in values_of(&out, "k").into_iter().filter(|v| v.is_finite()) {
            assert!((0.0..=100.0).contains(&v));
        }
        let plot_keys: Vec<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(plot_keys, vec!["k", "d", "obAnchor", "osAnchor"]);
    }

    #[test]
    fn williams_r_stays_between_minus_100_and_0() {
        let bars = synthetic(300);
        let s = spec("williams-percent-r");
        let out = s.compute(&bars, &s.default_inputs());
        let wr = values_of(&out, "wr");
        assert!(wr.iter().any(|v| v.is_finite()));
        for v in wr.into_iter().filter(|v| v.is_finite()) {
            assert!((-100.0..=0.0).contains(&v));
        }
    }

    #[test]
    fn ppo_histogram_is_optional_and_leads_the_plots() {
        let bars = synthetic(200);
        let s = spec("ppo");
        let with = s.compute(&bars, &s.default_inputs());
        assert_eq!(with.plots[0].key, "hist");
        let mut inputs = s.default_inputs();
        inputs.insert("showHistogram".into(), InputValue::from(false));
        let without = s.compute(&bars, &inputs);
        assert_eq!(without.plots[0].key, "ppo");
        assert_eq!(without.plots.len() + 1, with.plots.len());
    }

    #[test]
    fn fisher_trigger_is_the_line_delayed_one_bar() {
        let bars = synthetic(100);
        let s = spec("fisher-transform");
        let out = s.compute(&bars, &s.default_inputs());
        let (fisher, trigger) = (values_of(&out, "fisher"), values_of(&out, "trigger"));
        assert!(fisher.iter().any(|v| v.is_finite()));
        for i in 1..bars.len() {
            assert!(same(trigger[i], fisher[i - 1]));
        }
    }

    #[test]
    fn centered_dpo_leaves_the_newest_bars_empty() {
        let bars = synthetic(100);
        let s = spec("detrended-price-oscillator");
        let mut inputs = s.default_inputs();
        inputs.insert("centered".into(), InputValue::from(true));
        let out = s.compute(&bars, &inputs);
        let dpo = values_of(&out, "dpo");
        assert!(dpo[99].is_nan());
        assert!(dpo[60].is_finite());
    }

    #[test]
    fn ttm_squeeze_fire_marks_a_squeeze_release() {
        let bars = synthetic(400);
        let s = spec("ttm-squeeze");
        let out = s.compute(&bars, &s.default_inputs());
        let keys: Vec<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, vec!["mom", "state", "fire"]);
        let state = values_of(&out, "state");
        let fire = values_of(&out, "fire");
        for (i, f) in fire.iter().enumerate() {
            if f.is_finite() {
                assert!(i > 0 && state[i].is_finite() && state[i - 1].is_finite());
            }
        }
        let mut inputs = s.default_inputs();
        inputs.insert("markFires".into(), InputValue::from(false));
        assert_eq!(s.compute(&bars, &inputs).plots.len(), 2);
    }

    #[test]
    fn schaff_trend_cycle_stays_on_its_0_to_100_scale() {
        let bars = synthetic(400);
        let s = spec("schaff-trend-cycle");
        let out = s.compute(&bars, &s.default_inputs());
        let stc = values_of(&out, "stc");
        assert!(stc.iter().any(|v| v.is_finite()));
        for v in stc.into_iter().filter(|v| v.is_finite()) {
            assert!((-1e-9..=100.0 + 1e-9).contains(&v));
        }
        let upper = s
            .inputs
            .iter()
            .find(|i| i.key == "upper")
            .expect("upper input");
        assert_eq!(upper.group.as_deref(), Some("Trigger Levels"));
    }
}
