//! Volatility studies: how much price moves, regardless of direction.

use crate::i18n::{MessageLike, msg};
use crate::palette::{BEARISH, BULLISH, NEUTRAL, WARNING};

use super::define::{
    ClassicBand, ClassicIndicatorSpec, ClassicLevel, ClassicOutput, ClassicPlot, flag, len, num,
    text,
};
use super::math::{
    closes, ema, highest, highs, lowest, lows, map, rma, shift, sma, source_values, stdev, sub,
    sum, true_range, wma, zip,
};
use super::shared::{
    BASE_TRANSPARENCY, BoundedOptions, CenteredOptions, DirectionInk, RampInk, STYLE, anchor_plot,
    baseline_gradient, bool_input, bounded_oscillator, centered_oscillator, color_input,
    float_input, gradient, int_input, length_input, option_input, source_input, transp,
};
use crate::model::{LineLikeKind, LineStyle};

/// Rising readings take the accent; contracting or flat volatility stays structural grey.
fn expansion_colors(values: &[f64], calm: &str, expanding: &str) -> Vec<Option<String>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            if !x.is_finite() {
                return None;
            }
            let prev = if i > 0 && values[i - 1].is_finite() {
                values[i - 1]
            } else {
                x
            };
            Some(if x > prev { expanding } else { calm }.to_string())
        })
        .collect()
}

/// Line plus a wash down to zero: the shared look of the one-sided volatility metrics.
fn washed_to_zero(
    key: &str,
    title: MessageLike,
    values: Vec<f64>,
    colors: Vec<Option<String>>,
    fallback: &str,
    value_transparency: f64,
    width: f64,
) -> ClassicOutput {
    let wash = baseline_gradient(
        &values,
        0.0,
        |_x, i| colors[i].clone().unwrap_or_else(|| fallback.to_string()),
        value_transparency,
        BASE_TRANSPARENCY,
    );
    let band = ClassicBand::new("wash", key, "zeroAnchor", transp(NEUTRAL, 100.0)).gradient(wash);
    let anchor = anchor_plot("zeroAnchor", "Zero Anchor", values.len(), 0.0);
    let plot = ClassicPlot::new(key, title, values, fallback)
        .width(width)
        .colors(colors);
    ClassicOutput {
        plots: vec![plot, anchor],
        bands: vec![band],
        ..Default::default()
    }
}

fn atr_spec() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "average-true-range",
        msg("indicator.averageTrueRange.title", "Average True Range"),
        false,
        vec![
            length_input(
                14.0,
                "length",
                msg("indicator.averageTrueRange.length", "Length"),
                5000.0,
                Some(msg("indicator.averageTrueRange.length.tooltip", "Number of bars used to average the true range.")),
            ),
            option_input(
                "smoothing",
                msg("indicator.averageTrueRange.smoothing", "Smoothing"),
                "RMA",
                &["RMA", "SMA", "EMA", "WMA"],
                Some(msg("indicator.averageTrueRange.smoothing.tooltip", "Moving average applied to the true range. RMA is Wilder's original smoothing used by the classic ATR.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.averageTrueRange.color", "ATR Line"),
                "color",
                Some(msg("indicator.averageTrueRange.color.tooltip", "Color of the ATR line while volatility contracts or holds flat.")),
            ),
            color_input(
                WARNING,
                msg("indicator.averageTrueRange.expandColor", "Expanding"),
                "expandColor",
                Some(msg("indicator.averageTrueRange.expandColor.tooltip", "Color of the ATR line while it rises \u{2014} volatility expanding bar to bar.")),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 14);
            let tr = true_range(bars);
            let values = match text(inputs, "smoothing", "RMA").as_str() {
                "SMA" => sma(&tr, window),
                "EMA" => ema(&tr, window),
                "WMA" => wma(&tr, window),
                _ => rma(&tr, window),
            };
            let calm = text(inputs, "color", NEUTRAL);
            let colors = expansion_colors(&values, &calm, &text(inputs, "expandColor", WARNING));
            washed_to_zero("atr", msg("indicator.averageTrueRange.plot.atr", "ATR"), values, colors, &calm, 75.0, 1.0)
        },
    )
    .short_title(msg("indicator.averageTrueRange.shortTitle", "ATR"))
}

fn historical_volatility() -> ClassicIndicatorSpec {
    let mut length = length_input(
        10.0,
        "length",
        msg("indicator.historicalVolatility.length", "Length"),
        5000.0,
        Some(msg(
            "indicator.historicalVolatility.length.tooltip",
            "Lookback window in bars for the standard deviation of close-to-close log returns.",
        )),
    );
    length.min = Some(2.0);
    ClassicIndicatorSpec::new(
        "historical-volatility",
        msg("indicator.historicalVolatility.title", "Historical Volatility"),
        false,
        vec![
            length,
            bool_input(
                "annualize",
                msg("indicator.historicalVolatility.annualize", "Annualize"),
                true,
                super::shared::SETTINGS,
                Some(msg("indicator.historicalVolatility.annualize.tooltip", "Scale the per-bar volatility to an annual figure by multiplying by the square root of the periods per year.")),
            ),
            int_input(
                "periodsPerYear",
                msg("indicator.historicalVolatility.periodsPerYear", "Periods Per Year"),
                252.0,
                1.0,
                100000.0,
                Some(msg("indicator.historicalVolatility.periodsPerYear.tooltip", "Number of periods in a year used for annualization: 252 for daily bars, 52 for weekly, 12 for monthly.")),
            ),
            color_input(
                WARNING,
                msg("indicator.historicalVolatility.color", "Volatility Line"),
                "color",
                Some(msg("indicator.historicalVolatility.color.tooltip", "Color of the line at its recent highs; it fades toward grey as volatility contracts.")),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 10);
            let n = window as f64;
            let log_closes = map(&closes(bars), f64::ln);
            let log_returns = sub(&log_closes, &shift(&log_closes, 1));
            // Sample (n - 1) standard deviation about the window mean.
            let mean_return = map(&sum(&log_returns, window), |s| s / n);
            let squared_sum = sum(&map(&log_returns, |r| r * r), window);
            let factor = if flag(inputs, "annualize", true) { num(inputs, "periodsPerYear", 252.0).sqrt() } else { 1.0 };
            let values = zip(&squared_sum, &mean_return, |sq, m| ((sq - n * m * m).max(0.0) / (n - 1.0)).sqrt() * factor * 100.0);
            // Intensity tracks where the reading sits in its own recent range.
            let low = lowest(&values, 100);
            let high = highest(&values, 100);
            let accent = text(inputs, "color", WARNING);
            let colors: Vec<Option<String>> = values.iter().enumerate().map(|(i, &x)| x.is_finite().then(|| gradient(x, low[i], high[i], NEUTRAL, &accent))).collect();
            washed_to_zero("hv", msg("indicator.historicalVolatility.plot.hv", "Historical Volatility"), values, colors, &accent, 75.0, 2.0)
        },
    )
    .short_title(msg("indicator.historicalVolatility.shortTitle", "HV"))
}

fn chaikin_volatility() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "chaikin-volatility",
        msg("indicator.chaikinVolatility.title", "Chaikin Volatility"),
        false,
        vec![
            length_input(
                10.0,
                "emaLength",
                msg("indicator.chaikinVolatility.emaLength", "EMA Length"),
                5000.0,
                Some(msg("indicator.chaikinVolatility.emaLength.tooltip", "Length of the exponential moving average applied to the bar high-low range.")),
            ),
            length_input(
                10.0,
                "rocLength",
                msg("indicator.chaikinVolatility.rocLength", "ROC Length"),
                5000.0,
                Some(msg("indicator.chaikinVolatility.rocLength.tooltip", "Number of bars used for the rate of change of the smoothed high-low range.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.chaikinVolatility.color", "CHV Color"),
                "color",
                Some(msg("indicator.chaikinVolatility.color.tooltip", "Color of the Chaikin Volatility line.")),
            ),
            color_input(
                WARNING,
                msg("indicator.chaikinVolatility.expansionColor", "Expansion"),
                "expansionColor",
                Some(msg(
                    "indicator.chaikinVolatility.expansionColor.tooltip",
                    "Line and fill color while the reading is above zero, when the smoothed range is wider than it was at the start of the ROC Length lookback.",
                )),
            ),
        ],
        |bars, inputs| {
            let range = ema(&sub(&highs(bars), &lows(bars)), len(inputs, "emaLength", 10));
            let prior = shift(&range, len(inputs, "rocLength", 10) as i64);
            let values = zip(&range, &prior, |r, p| if p == 0.0 { f64::NAN } else { (100.0 * (r - p)) / p });
            let calm = text(inputs, "color", NEUTRAL);
            let expansion = text(inputs, "expansionColor", WARNING);
            // Only expansion is a state worth accenting; contraction stays structural grey.
            let ink = DirectionInk { bull: expansion, bear: calm.clone(), neutral: calm };
            let parts = centered_oscillator(CenteredOptions::new("chv", msg("indicator.chaikinVolatility.plot.chv", "Chaikin Volatility"), values, ink, NEUTRAL));
            ClassicOutput { plots: parts.plots, bands: parts.bands, levels: parts.levels, ..Default::default() }
        },
    )
    .short_title(msg("indicator.chaikinVolatility.shortTitle", "CHV"))
}

fn mass_index() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "mass-index",
        msg("indicator.massIndex.title", "Mass Index"),
        false,
        vec![
            length_input(
                25.0,
                "length",
                msg("indicator.massIndex.length", "Sum Length"),
                5000.0,
                Some(msg("indicator.massIndex.length.tooltip", "Number of bars over which the single/double EMA ratio is summed.")),
            ),
            length_input(
                9.0,
                "emaLength",
                msg("indicator.massIndex.emaLength", "EMA Length"),
                5000.0,
                Some(msg("indicator.massIndex.emaLength.tooltip", "Length of the single EMA of the high-low range and of its double smoothing.")),
            ),
            float_input(
                "setupLevel",
                msg("indicator.massIndex.setupLevel", "Setup Level"),
                27.0,
                0.0,
                1000.0,
                0.1,
                Some(msg("indicator.massIndex.setupLevel.tooltip", "Reversal bulge setup level; a rise above it arms the pattern.")),
            ),
            float_input(
                "triggerLevel",
                msg("indicator.massIndex.triggerLevel", "Trigger Level"),
                26.5,
                0.0,
                1000.0,
                0.1,
                Some(msg("indicator.massIndex.triggerLevel.tooltip", "Reversal bulge trigger level; a decline below it after a setup completes the pattern.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.massIndex.color", "Mass Index"),
                "color",
                Some(msg("indicator.massIndex.color.tooltip", "Line color while no reversal bulge is armed.")),
            ),
            color_input(
                WARNING,
                msg("indicator.massIndex.armedColor", "Armed"),
                "armedColor",
                Some(msg("indicator.massIndex.armedColor.tooltip", "Line color from the rise above the setup level until the decline below the trigger level.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.massIndex.levelsColor", "Levels"),
                "levelsColor",
                Some(msg("indicator.massIndex.levelsColor.tooltip", "Reversal bulge levels color.")),
            ),
        ],
        |bars, inputs| {
            let ema_len = len(inputs, "emaLength", 9);
            let single = ema(&sub(&highs(bars), &lows(bars)), ema_len);
            // A flat market yields the neutral ratio 1 rather than a gap.
            let ratio = zip(&single, &ema(&single, ema_len), |a, b| if b == 0.0 { 1.0 } else { a / b });
            let values = sum(&ratio, len(inputs, "length", 25));
            let setup = num(inputs, "setupLevel", 27.0);
            let trigger = num(inputs, "triggerLevel", 26.5);
            let calm = text(inputs, "color", NEUTRAL);
            let armed_ink = text(inputs, "armedColor", WARNING);
            let level_ink = text(inputs, "levelsColor", NEUTRAL);
            // Armed by the rise above the setup level, cleared by the decline below the trigger.
            let mut armed = false;
            let colors: Vec<Option<String>> = values
                .iter()
                .map(|&x| {
                    if !x.is_finite() {
                        return None;
                    }
                    armed = if x > setup {
                        true
                    } else if x < trigger {
                        false
                    } else {
                        armed
                    };
                    Some(if armed { armed_ink.clone() } else { calm.clone() })
                })
                .collect();
            let n = values.len();
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new("mi", msg("indicator.massIndex.plot.mi", "Mass Index"), values, calm).colors(colors),
                    anchor_plot("setupAnchor", "Setup Anchor", n, setup),
                    anchor_plot("triggerAnchor", "Trigger Anchor", n, trigger),
                ],
                bands: vec![ClassicBand::new("bulge", "setupAnchor", "triggerAnchor", transp(&armed_ink, 90.0))],
                levels: vec![
                    ClassicLevel::new("setup", setup).color(level_ink.clone()).line_style(LineStyle::Dashed).title(msg("indicator.massIndex.level.setup", "Setup Level")),
                    ClassicLevel::new("trigger", trigger).color(level_ink).line_style(LineStyle::Dashed).title(msg("indicator.massIndex.level.trigger", "Trigger Level")),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.massIndex.shortTitle", "MI"))
}

fn standard_deviation() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "standard-deviation",
        msg("indicator.standardDeviation.title", "Standard Deviation"),
        false,
        vec![
            length_input(
                20.0,
                "length",
                msg("indicator.standardDeviation.length", "Length"),
                5000.0,
                Some(msg(
                    "indicator.standardDeviation.length.tooltip",
                    "Number of bars used to compute the standard deviation.",
                )),
            ),
            source_input(
                "Close",
                "source",
                msg("indicator.standardDeviation.source", "Source"),
                Some(msg(
                    "indicator.standardDeviation.source.tooltip",
                    "Price series the standard deviation is calculated on.",
                )),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.standardDeviation.color", "Standard Deviation"),
                "color",
                Some(msg(
                    "indicator.standardDeviation.color.tooltip",
                    "Line color while volatility contracts or holds flat.",
                )),
            ),
            color_input(
                WARNING,
                msg("indicator.standardDeviation.expandColor", "Expanding"),
                "expandColor",
                Some(msg(
                    "indicator.standardDeviation.expandColor.tooltip",
                    "Line and fill color while volatility expands bar to bar.",
                )),
            ),
        ],
        |bars, inputs| {
            let values = stdev(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "length", 20),
            );
            let calm = text(inputs, "color", NEUTRAL);
            let colors = expansion_colors(&values, &calm, &text(inputs, "expandColor", WARNING));
            washed_to_zero(
                "stdev",
                msg(
                    "indicator.standardDeviation.plot.stdev",
                    "Standard Deviation",
                ),
                values,
                colors,
                &calm,
                80.0,
                1.0,
            )
        },
    )
    .short_title(msg("indicator.standardDeviation.shortTitle", "StdDev"))
}

fn ulcer_index() -> ClassicIndicatorSpec {
    let mut length = length_input(
        14.0,
        "length",
        msg("indicator.ulcerIndex.length", "Length"),
        500.0,
        Some(msg(
            "indicator.ulcerIndex.length.tooltip",
            "Lookback window. At each bar the percentage drawdown from the window's running maximum is squared; the squares are averaged and the root taken.",
        )),
    );
    length.min = Some(2.0);
    ClassicIndicatorSpec::new(
        "ulcer-index",
        msg("indicator.ulcerIndex.title", "Ulcer Index"),
        false,
        vec![
            length,
            source_input(
                "Close",
                "source",
                msg("indicator.ulcerIndex.source", "Source"),
                Some(msg(
                    "indicator.ulcerIndex.source.tooltip",
                    "Price series the Ulcer Index is computed on. Close is the standard choice.",
                )),
            ),
            float_input(
                "threshold",
                msg("indicator.ulcerIndex.threshold", "Stress Threshold"),
                5.0,
                0.0,
                1000.0,
                0.25,
                Some(msg(
                    "indicator.ulcerIndex.threshold.tooltip",
                    "Reference level drawn as a dashed line. The index scales with the instrument's volatility and the window length, so set it relative to its own history.",
                )),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.ulcerIndex.color", "Ulcer Index"),
                "color",
                Some(msg(
                    "indicator.ulcerIndex.color.tooltip",
                    "Color of the index near zero; it blends toward the stress color as the reading approaches the threshold.",
                )),
            ),
            color_input(
                BEARISH,
                msg("indicator.ulcerIndex.stressColor", "Stress"),
                "stressColor",
                Some(msg(
                    "indicator.ulcerIndex.stressColor.tooltip",
                    "Color of the index at and beyond the stress threshold.",
                )),
            ),
            bool_input(
                "gradientFill",
                msg("indicator.ulcerIndex.gradientFill", "Gradient Fill"),
                true,
                STYLE,
                Some(msg(
                    "indicator.ulcerIndex.gradientFill.tooltip",
                    "Vertical gradient between zero and the Ulcer Index, fully transparent at zero.",
                )),
            ),
            bool_input(
                "showDrawdown",
                msg("indicator.ulcerIndex.showDrawdown", "Drawdown %"),
                false,
                STYLE,
                Some(msg(
                    "indicator.ulcerIndex.showDrawdown.tooltip",
                    "Plot the current percentage drawdown from the window high as columns below zero \u{2014} the raw ingredient the index averages.",
                )),
            ),
            color_input(
                &transp(BEARISH, 50.0),
                msg("indicator.ulcerIndex.drawdownColor", "Drawdown Color"),
                "drawdownColor",
                Some(msg(
                    "indicator.ulcerIndex.drawdownColor.tooltip",
                    "Color of the drawdown columns.",
                )),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.ulcerIndex.thresholdColor", "Threshold"),
                "thresholdColor",
                Some(msg(
                    "indicator.ulcerIndex.thresholdColor.tooltip",
                    "Color of the stress threshold line.",
                )),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 14);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let n = src.len();
            let mut values = vec![f64::NAN; n];
            let mut drawdown = vec![f64::NAN; n];
            for i in window.saturating_sub(1)..n {
                // The running maximum walks forward from the start of the window, so an early
                // peak keeps scoring every later bar as underwater.
                let mut run_max = f64::NAN;
                let mut sum_sq = 0.0;
                let mut valid = true;
                let mut last = f64::NAN;
                for &price in &src[i + 1 - window..=i] {
                    if !price.is_finite() {
                        valid = false;
                        break;
                    }
                    run_max = if run_max.is_finite() {
                        run_max.max(price)
                    } else {
                        price
                    };
                    if run_max <= 0.0 {
                        valid = false;
                        break;
                    }
                    last = (100.0 * (price - run_max)) / run_max;
                    sum_sq += last * last;
                }
                if !valid {
                    continue;
                }
                values[i] = (sum_sq / window as f64).sqrt();
                drawdown[i] = last;
            }
            let calm = text(inputs, "color", NEUTRAL);
            let stress = text(inputs, "stressColor", BEARISH);
            let threshold = num(inputs, "threshold", 5.0);
            let colors: Vec<Option<String>> = values
                .iter()
                .map(|&x| {
                    x.is_finite().then(|| {
                        if threshold > 0.0 {
                            gradient(x, 0.0, threshold, &calm, &stress)
                        } else {
                            stress.clone()
                        }
                    })
                })
                .collect();
            let mut bands = Vec::new();
            let mut anchor = None;
            if flag(inputs, "gradientFill", true) {
                anchor = Some(anchor_plot("zeroAnchor", "Zero Base", n, 0.0));
                let wash = baseline_gradient(
                    &values,
                    0.0,
                    |_x, i| colors[i].clone().unwrap_or_else(|| calm.clone()),
                    super::shared::VALUE_TRANSPARENCY,
                    BASE_TRANSPARENCY,
                );
                bands.push(
                    ClassicBand::new("wash", "ui", "zeroAnchor", transp(NEUTRAL, 100.0))
                        .gradient(wash),
                );
            }
            let mut plots = vec![
                ClassicPlot::new(
                    "ui",
                    msg("indicator.ulcerIndex.plot.ui", "Ulcer Index"),
                    values,
                    calm,
                )
                .colors(colors),
            ];
            plots.extend(anchor);
            if flag(inputs, "showDrawdown", false) {
                plots.push(
                    ClassicPlot::new(
                        "dd",
                        msg("indicator.ulcerIndex.plot.dd", "Drawdown %"),
                        drawdown,
                        text(inputs, "drawdownColor", &transp(BEARISH, 50.0)),
                    )
                    .kind(LineLikeKind::Columns)
                    .base(0.0),
                );
            }
            ClassicOutput {
                plots,
                bands,
                levels: vec![
                    ClassicLevel::new("zero", 0.0)
                        .color(transp(NEUTRAL, 50.0))
                        .line_style(LineStyle::Dotted)
                        .title(msg("indicator.ulcerIndex.level.zero", "Zero Line")),
                    ClassicLevel::new("threshold", threshold)
                        .color(text(inputs, "thresholdColor", NEUTRAL))
                        .line_style(LineStyle::Dashed)
                        .title(msg(
                            "indicator.ulcerIndex.level.threshold",
                            "Stress Threshold",
                        )),
                ],
                ..Default::default()
            }
        },
    )
}

fn choppiness() -> ClassicIndicatorSpec {
    let mut length = length_input(
        14.0,
        "length",
        msg("indicator.choppinessIndex.length", "Length"),
        5000.0,
        Some(msg(
            "indicator.choppinessIndex.length.tooltip",
            "Number of bars used to sum true ranges and to measure the highest high \u{2212} lowest low range.",
        )),
    );
    length.min = Some(2.0);
    ClassicIndicatorSpec::new(
        "choppiness-index",
        msg("indicator.choppinessIndex.title", "Choppiness Index"),
        false,
        vec![
            length,
            float_input(
                "upperBand",
                msg("indicator.choppinessIndex.upperBand", "Upper Band"),
                61.8,
                0.0,
                100.0,
                0.1,
                Some(msg("indicator.choppinessIndex.upperBand.tooltip", "Readings above this level indicate a choppy, range-bound market.")),
            ),
            float_input(
                "lowerBand",
                msg("indicator.choppinessIndex.lowerBand", "Lower Band"),
                38.2,
                0.0,
                100.0,
                0.1,
                Some(msg("indicator.choppinessIndex.lowerBand.tooltip", "Readings below this level indicate a strong trend.")),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.choppinessIndex.color", "CHOP Line"),
                "color",
                Some(msg("indicator.choppinessIndex.color.tooltip", "Color of the Choppiness Index line.")),
            ),
            color_input(
                BULLISH,
                msg("indicator.choppinessIndex.trendingColor", "Trending"),
                "trendingColor",
                Some(msg("indicator.choppinessIndex.trendingColor.tooltip", "Line, fill and zone color toward and below the lower band \u{2014} a trending market.")),
            ),
            color_input(
                BEARISH,
                msg("indicator.choppinessIndex.choppyColor", "Choppy"),
                "choppyColor",
                Some(msg("indicator.choppinessIndex.choppyColor.tooltip", "Line, fill and zone color toward and above the upper band \u{2014} a choppy, range-bound market.")),
            ),
            color_input(
                &transp(NEUTRAL, 90.0),
                msg("indicator.choppinessIndex.fillColor", "Band Fill"),
                "fillColor",
                Some(msg("indicator.choppinessIndex.fillColor.tooltip", "Fill color of the zone between the upper and lower bands.")),
            ),
        ],
        |bars, inputs| {
            let window = len(inputs, "length", 14);
            let tr_sum = sum(&true_range(bars), window);
            let span = zip(&highest(&highs(bars), window), &lowest(&lows(bars), window), |h, l| h - l);
            let n = window as f64;
            let values = zip(&tr_sum, &span, |t, s| if s > 0.0 && t > 0.0 { (100.0 * (t / s).log10()) / n.log10() } else { f64::NAN });
            let parts = bounded_oscillator(BoundedOptions {
                key: "chop".into(),
                title: msg("indicator.choppinessIndex.plot.chop", "CHOP"),
                values,
                overbought: num(inputs, "upperBand", 61.8),
                oversold: num(inputs, "lowerBand", 38.2),
                scale_top: 100.0,
                scale_bottom: 0.0,
                ink: RampInk { low: text(inputs, "trendingColor", BULLISH), mid: text(inputs, "color", NEUTRAL), high: text(inputs, "choppyColor", BEARISH) },
                level_ink: NEUTRAL.into(),
                width: None,
                zone_transparency: None,
                midline: None,
                band_fill: Some(text(inputs, "fillColor", &transp(NEUTRAL, 90.0))),
                midpoint_wash: true,
                level_titles: Some((msg("indicator.choppinessIndex.level.upperBand", "Upper Band"), msg("indicator.choppinessIndex.level.lowerBand", "Lower Band"))),
            });
            ClassicOutput { plots: parts.plots, bands: parts.bands, levels: parts.levels, ..Default::default() }
        },
    )
    .short_title(msg("indicator.choppinessIndex.shortTitle", "CHOP"))
}

pub fn volatility_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        atr_spec(),
        historical_volatility(),
        chaikin_volatility(),
        mass_index(),
        standard_deviation(),
        ulcer_index(),
        choppiness(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::source_text;
    use crate::model::Ohlcv;

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
        volatility_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    #[test]
    fn declares_every_volatility_study_in_order() {
        let types: Vec<&str> = volatility_specs()
            .iter()
            .map(|s| s.indicator_type)
            .collect();
        assert_eq!(
            types,
            [
                "average-true-range",
                "historical-volatility",
                "chaikin-volatility",
                "mass-index",
                "standard-deviation",
                "ulcer-index",
                "choppiness-index"
            ]
        );
    }

    #[test]
    fn every_spec_computes_aligned_plots_on_default_inputs() {
        let bars = wave(400);
        for spec in volatility_specs() {
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
                out.plots[0].values.iter().any(|v| v.is_finite()),
                "{} has no finite value",
                spec.indicator_type
            );
        }
    }

    #[test]
    fn short_titles_match_the_catalog() {
        let expected = [
            ("average-true-range", "ATR"),
            ("historical-volatility", "HV"),
            ("chaikin-volatility", "CHV"),
            ("mass-index", "MI"),
            ("standard-deviation", "StdDev"),
            ("ulcer-index", "Ulcer Index"),
            ("choppiness-index", "CHOP"),
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
    fn atr_of_constant_range_bars_is_that_range() {
        let bars: Vec<Ohlcv> = (0..40).map(|i| bar(100.0, i)).collect();
        let spec = spec("average-true-range");
        let out = spec.compute(&bars, &spec.default_inputs());
        let atr = &out.plots[0].values;
        assert!(atr[..13].iter().all(|v| v.is_nan()));
        assert!(atr[13..].iter().all(|v| (v - 2.0).abs() < 1e-12));
        assert_eq!(out.plots[1].key, "zeroAnchor");
        assert_eq!(out.bands[0].to, "zeroAnchor");
    }

    #[test]
    fn standard_deviation_of_a_flat_series_is_zero_and_never_expanding() {
        let bars: Vec<Ohlcv> = (0..40).map(|i| bar(100.0, i)).collect();
        let spec = spec("standard-deviation");
        let out = spec.compute(&bars, &spec.default_inputs());
        assert!(out.plots[0].values[19..].iter().all(|v| *v == 0.0));
        let colors = out.plots[0].colors.as_ref().expect("colors");
        assert!(colors[19..].iter().all(|c| c.as_deref() == Some(NEUTRAL)));
    }

    #[test]
    fn ulcer_index_is_zero_on_a_rising_series_and_positive_after_a_drop() {
        let rising: Vec<Ohlcv> = (0..30).map(|i| bar(100.0 + i as f64, i)).collect();
        let spec = spec("ulcer-index");
        let out = spec.compute(&rising, &spec.default_inputs());
        assert!(out.plots[0].values[13..].iter().all(|v| *v == 0.0));
        let mut dropped = rising;
        dropped.push(bar(50.0, 30));
        let out = spec.compute(&dropped, &spec.default_inputs());
        assert!(out.plots[0].values[30] > 0.0);
    }

    #[test]
    fn ulcer_index_optional_plots_follow_their_toggles() {
        let bars = wave(60);
        let spec = spec("ulcer-index");
        let mut inputs = spec.default_inputs();
        assert_eq!(
            spec.compute(&bars, &inputs)
                .plots
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>(),
            ["ui", "zeroAnchor"]
        );
        inputs.insert("gradientFill".into(), false.into());
        inputs.insert("showDrawdown".into(), true.into());
        let out = spec.compute(&bars, &inputs);
        assert_eq!(
            out.plots.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            ["ui", "dd"]
        );
        assert!(out.bands.is_empty());
        assert!(
            out.plots[1]
                .values
                .iter()
                .filter(|v| v.is_finite())
                .all(|v| *v <= 0.0)
        );
    }

    #[test]
    fn mass_index_arms_above_setup_and_levels_follow_inputs() {
        let bars = wave(200);
        let spec = spec("mass-index");
        let mut inputs = spec.default_inputs();
        inputs.insert("setupLevel".into(), 0.5.into());
        inputs.insert("triggerLevel".into(), 0.1.into());
        let out = spec.compute(&bars, &inputs);
        let colors = out.plots[0].colors.as_ref().expect("colors");
        assert!(colors.iter().flatten().all(|c| c == WARNING));
        assert_eq!(out.levels[0].price, 0.5);
        assert_eq!(out.levels[1].price, 0.1);
    }

    #[test]
    fn chaikin_volatility_is_a_centered_oscillator() {
        let bars = wave(100);
        let spec = spec("chaikin-volatility");
        let out = spec.compute(&bars, &spec.default_inputs());
        assert_eq!(
            out.plots.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            ["chv", "baseline"]
        );
        assert_eq!(out.levels[0].key, "zero");
    }

    #[test]
    fn choppiness_levels_use_band_titles() {
        let bars = wave(100);
        let spec = spec("choppiness-index");
        let out = spec.compute(&bars, &spec.default_inputs());
        assert_eq!(
            out.levels
                .iter()
                .map(|l| l.key.as_str())
                .collect::<Vec<_>>(),
            ["overbought", "oversold"]
        );
        assert_eq!(
            source_text(out.levels[0].title.as_ref().expect("title")),
            "Upper Band"
        );
        assert!(
            out.plots[0]
                .values
                .iter()
                .filter(|v| v.is_finite())
                .all(|v| (0.0..=100.0).contains(v))
        );
    }
}
