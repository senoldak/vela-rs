//! Volume studies. All of them read the bars' own volume; bars without a volume
//! report become gaps (never fake zeros), which the math module's NaN handling
//! carries through every derived line.

use crate::i18n::{MessageLike, msg};
use crate::model::{FillGradientStop, LineLikeKind, Ohlcv, SeriesDisplay};
use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL, WARNING};

use super::define::{
    ClassicBand, ClassicIndicatorSpec, ClassicInput, ClassicOutput, ClassicPlot, flag, len, num,
    text,
};
use super::math::{
    change, closes, cum_sum, ema, js_max, js_min, map, rma, shift, sma, source_values, stdev, sub,
    sum, volumes, wma, zip,
};
use super::shared::{
    BoundedOptions, CenteredOptions, DirectionInk, MAX_LENGTH, Parts, RampInk, SETTINGS, STYLE,
    bool_input, bounded_oscillator, centered_oscillator, color_input, float_input, int_input,
    length_input, option_input, source_input, transp,
};

const NAN: f64 = f64::NAN;

/// Close-location value × volume (the accumulation/distribution money-flow term).
fn money_flow_volume(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter()
        .map(|b| match b.volume {
            Some(v) if v.is_finite() => {
                if b.high == b.low {
                    0.0
                } else {
                    ((2.0 * b.close - b.high - b.low) / (b.high - b.low)) * v
                }
            }
            _ => NAN,
        })
        .collect()
}

/// Per-bar ink from the line's own slope, holding the last color through a flat bar.
fn slope_colors(values: &[f64], bull: &str, bear: &str, flat: &str) -> Vec<Option<String>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            if !x.is_finite() {
                return None;
            }
            let prev = if i > 0 { values[i - 1] } else { NAN };
            if !prev.is_finite() {
                return Some(flat.to_string());
            }
            Some(
                if x > prev {
                    bull
                } else if x < prev {
                    bear
                } else {
                    flat
                }
                .to_string(),
            )
        })
        .collect()
}

/// A cumulative index seeded at `base` that compounds only on the sessions `when` accepts.
fn volume_index(
    bars: &[Ohlcv],
    src: &[f64],
    base: f64,
    when: impl Fn(f64, f64) -> bool,
) -> Vec<f64> {
    let mut out = vec![NAN; bars.len()];
    let mut index = base;
    for i in 0..bars.len() {
        let v = bars[i].volume;
        let pv = if i > 0 { bars[i - 1].volume } else { None };
        let prev = if i > 0 { src[i - 1] } else { NAN };
        let active = match (v, pv) {
            (Some(v), Some(pv)) => v.is_finite() && pv.is_finite() && when(v, pv),
            _ => false,
        };
        if active && prev.is_finite() && prev != 0.0 {
            index *= 1.0 + (src[i] - prev) / prev;
        }
        out[i] = index;
    }
    out
}

/// The inks of the NVI/PVI regime look.
struct RegimeInk {
    bull: String,
    bear: String,
    neutral: String,
    signal: String,
}

/// The NVI/PVI look: a cumulative line colored by its side of a long signal average,
/// washed toward that average. Both indices differ only in which sessions move them.
#[allow(clippy::too_many_arguments)]
fn regime_index(
    key: &str,
    title: &'static str,
    values: Vec<f64>,
    signal: Vec<f64>,
    ink: &RegimeInk,
    show_signal: bool,
    gradient_fill: bool,
    single_color: bool,
) -> Parts {
    let ink_of = |i: usize| -> &str {
        let s = signal[i];
        let x = values[i];
        if single_color || !s.is_finite() {
            &ink.neutral
        } else if x > s {
            &ink.bull
        } else if x < s {
            &ink.bear
        } else {
            &ink.neutral
        }
    };
    let colors: Vec<Option<String>> = values
        .iter()
        .enumerate()
        .map(|(i, x)| x.is_finite().then(|| ink_of(i).to_string()))
        .collect();
    let mut signal_plot = ClassicPlot::new(
        "signal",
        msg("indicator.regime.plot.signal", "Signal EMA"),
        signal.clone(),
        ink.signal.clone(),
    );
    if !show_signal {
        signal_plot = signal_plot.display(SeriesDisplay {
            pane: Some(false),
            ..Default::default()
        });
    }
    let plots = vec![
        signal_plot,
        ClassicPlot::new(key, title, values.clone(), ink.neutral.clone())
            .width(2.0)
            .colors(colors),
    ];
    let mut bands = Vec::new();
    if gradient_fill {
        let gradient: Vec<Option<FillGradientStop>> = values
            .iter()
            .zip(&signal)
            .map(|(&x, &s)| {
                if !x.is_finite() || !s.is_finite() {
                    return None;
                }
                let above = x > s;
                let c = if above { &ink.bull } else { &ink.bear };
                Some(FillGradientStop {
                    top_value: js_max(x, s),
                    bottom_value: js_min(x, s),
                    top_color: transp(c, if above { 50.0 } else { 100.0 }),
                    bottom_color: transp(c, if above { 100.0 } else { 50.0 }),
                })
            })
            .collect();
        bands.push(
            ClassicBand::new("regime", key, "signal", transp(NEUTRAL, 100.0)).gradient(gradient),
        );
    }
    Parts {
        plots,
        bands,
        levels: Vec::new(),
    }
}

fn output(parts: Parts) -> ClassicOutput {
    ClassicOutput {
        plots: parts.plots,
        bands: parts.bands,
        levels: parts.levels,
        ..Default::default()
    }
}

fn ink3(
    bull: impl Into<String>,
    bear: impl Into<String>,
    neutral: impl Into<String>,
) -> DirectionInk {
    DirectionInk {
        bull: bull.into(),
        bear: bear.into(),
        neutral: neutral.into(),
    }
}

fn tip(id: &'static str, text: &'static str) -> Option<MessageLike> {
    Some(msg(id, text))
}

fn on_balance_volume() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "on-balance-volume",
        msg("indicator.onBalanceVolume.title", "On Balance Volume"),
        false,
        vec![
            bool_input(
                "smoothing",
                msg("indicator.onBalanceVolume.smoothing", "Smoothing MA"),
                false,
                SETTINGS,
                tip(
                    "indicator.onBalanceVolume.smoothing.tooltip",
                    "Display a moving average of OBV to act as a smoothing/signal line.",
                ),
            ),
            option_input(
                "maType",
                msg("indicator.onBalanceVolume.maType", "MA Type"),
                "SMA",
                &["SMA", "EMA", "WMA", "RMA (SMMA)"],
                tip(
                    "indicator.onBalanceVolume.maType.tooltip",
                    "Moving average type used for the smoothing MA.",
                ),
            ),
            length_input(
                20.0,
                "maLength",
                msg("indicator.onBalanceVolume.maLength", "MA Length"),
                MAX_LENGTH,
                tip(
                    "indicator.onBalanceVolume.maLength.tooltip",
                    "Number of bars used for the smoothing MA.",
                ),
            ),
            color_input(
                INFO,
                msg("indicator.onBalanceVolume.color", "OBV"),
                "color",
                tip(
                    "indicator.onBalanceVolume.color.tooltip",
                    "Color of the OBV line.",
                ),
            ),
            color_input(
                WARNING,
                msg("indicator.onBalanceVolume.maColor", "Smoothing MA"),
                "maColor",
                tip(
                    "indicator.onBalanceVolume.maColor.tooltip",
                    "Color of the smoothing MA line.",
                ),
            ),
        ],
        |bars, inputs| {
            let vol = volumes(bars);
            let d = change(&closes(bars), 1);
            let values = cum_sum(&zip(&vol, &d, |v, x| {
                if x > 0.0 {
                    v
                } else if x < 0.0 {
                    -v
                } else {
                    0.0
                }
            }));
            let mut plots = vec![ClassicPlot::new(
                "obv",
                msg("indicator.onBalanceVolume.plot.obv", "OBV"),
                values.clone(),
                text(inputs, "color", INFO),
            )];
            if flag(inputs, "smoothing", false) {
                let n = len(inputs, "maLength", 20);
                let ma = match text(inputs, "maType", "SMA").as_str() {
                    "EMA" => ema(&values, n),
                    "WMA" => wma(&values, n),
                    "RMA (SMMA)" => rma(&values, n),
                    _ => sma(&values, n),
                };
                plots.push(ClassicPlot::new(
                    "ma",
                    msg("indicator.onBalanceVolume.plot.ma", "Smoothing MA"),
                    ma,
                    text(inputs, "maColor", WARNING),
                ));
            }
            ClassicOutput {
                plots,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.onBalanceVolume.shortTitle", "OBV"))
}

fn accumulation_distribution() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "accumulation-distribution",
        msg("indicator.accumulationDistribution.title", "Accumulation / Distribution"),
        false,
        vec![
            bool_input(
                "slopeColoring",
                msg("indicator.accumulationDistribution.slopeColoring", "Slope Coloring"),
                true,
                STYLE,
                tip(
                    "indicator.accumulationDistribution.slopeColoring.tooltip",
                    "Color the line by its slope — bullish while rising (accumulation), bearish while falling (distribution).",
                ),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.accumulationDistribution.color", "A/D Line"),
                "color",
                tip("indicator.accumulationDistribution.color.tooltip", "Color of the line when slope coloring is disabled or the line is flat."),
            ),
            color_input(
                BULLISH,
                msg("indicator.accumulationDistribution.bullColor", "Rising"),
                "bullColor",
                tip("indicator.accumulationDistribution.bullColor.tooltip", "Line color while the A/D Line rises."),
            ),
            color_input(
                BEARISH,
                msg("indicator.accumulationDistribution.bearColor", "Falling"),
                "bearColor",
                tip("indicator.accumulationDistribution.bearColor.tooltip", "Line color while the A/D Line falls."),
            ),
        ],
        |bars, inputs| {
            let values = cum_sum(&money_flow_volume(bars));
            let flat = text(inputs, "color", NEUTRAL);
            let mut plot = ClassicPlot::new("ad", msg("indicator.accumulationDistribution.plot.ad", "A/D"), values.clone(), flat.clone());
            if flag(inputs, "slopeColoring", true) {
                plot = plot.colors(slope_colors(&values, &text(inputs, "bullColor", BULLISH), &text(inputs, "bearColor", BEARISH), &flat));
            }
            ClassicOutput { plots: vec![plot], ..Default::default() }
        },
    )
    .short_title(msg("indicator.accumulationDistribution.shortTitle", "A/D"))
}

fn chaikin_money_flow() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "chaikin-money-flow",
        msg("indicator.chaikinMoneyFlow.title", "Chaikin Money Flow"),
        false,
        vec![
            length_input(
                20.0,
                "length",
                msg("indicator.chaikinMoneyFlow.length", "Length"),
                MAX_LENGTH,
                tip(
                    "indicator.chaikinMoneyFlow.length.tooltip",
                    "Lookback length over which money flow volume and volume are summed.",
                ),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.chaikinMoneyFlow.color", "CMF Line"),
                "color",
                tip(
                    "indicator.chaikinMoneyFlow.color.tooltip",
                    "Color of the Chaikin Money Flow line.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.chaikinMoneyFlow.bullColor", "Bullish"),
                "bullColor",
                tip(
                    "indicator.chaikinMoneyFlow.bullColor.tooltip",
                    "Line and fill color while the CMF reads above zero (net money flowing in).",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.chaikinMoneyFlow.bearColor", "Bearish"),
                "bearColor",
                tip(
                    "indicator.chaikinMoneyFlow.bearColor.tooltip",
                    "Line and fill color while the CMF reads below zero (net money flowing out).",
                ),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.chaikinMoneyFlow.zeroColor", "Zero Line"),
                "zeroColor",
                tip(
                    "indicator.chaikinMoneyFlow.zeroColor.tooltip",
                    "Color of the zero reference line.",
                ),
            ),
        ],
        |bars, inputs| {
            let n = len(inputs, "length", 20);
            let values = zip(
                &sum(&money_flow_volume(bars), n),
                &sum(&volumes(bars), n),
                |m, v| if v == 0.0 { 0.0 } else { m / v },
            );
            output(centered_oscillator(CenteredOptions::new(
                "cmf",
                msg("indicator.chaikinMoneyFlow.plot.cmf", "CMF"),
                values,
                ink3(
                    text(inputs, "bullColor", BULLISH),
                    text(inputs, "bearColor", BEARISH),
                    text(inputs, "color", NEUTRAL),
                ),
                text(inputs, "zeroColor", NEUTRAL),
            )))
        },
    )
    .short_title(msg("indicator.chaikinMoneyFlow.shortTitle", "CMF"))
}

fn chaikin_oscillator() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "chaikin-oscillator",
        msg("indicator.chaikinOscillator.title", "Chaikin Oscillator"),
        false,
        vec![
            length_input(
                3.0,
                "fastLength",
                msg("indicator.chaikinOscillator.fastLength", "Fast Length"),
                MAX_LENGTH,
                tip(
                    "indicator.chaikinOscillator.fastLength.tooltip",
                    "Length of the fast EMA applied to the accumulation/distribution line.",
                ),
            ),
            length_input(
                10.0,
                "slowLength",
                msg("indicator.chaikinOscillator.slowLength", "Slow Length"),
                MAX_LENGTH,
                tip(
                    "indicator.chaikinOscillator.slowLength.tooltip",
                    "Length of the slow EMA applied to the accumulation/distribution line.",
                ),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.chaikinOscillator.color", "Oscillator"),
                "color",
                tip(
                    "indicator.chaikinOscillator.color.tooltip",
                    "Color of the Chaikin Oscillator line.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.chaikinOscillator.bullColor", "Bullish"),
                "bullColor",
                tip(
                    "indicator.chaikinOscillator.bullColor.tooltip",
                    "Line and fill color while the oscillator reads above zero (accumulation).",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.chaikinOscillator.bearColor", "Bearish"),
                "bearColor",
                tip(
                    "indicator.chaikinOscillator.bearColor.tooltip",
                    "Line and fill color while the oscillator reads below zero (distribution).",
                ),
            ),
        ],
        |bars, inputs| {
            let ad = cum_sum(&money_flow_volume(bars));
            let values = sub(
                &ema(&ad, len(inputs, "fastLength", 3)),
                &ema(&ad, len(inputs, "slowLength", 10)),
            );
            output(centered_oscillator(CenteredOptions::new(
                "co",
                msg("indicator.chaikinOscillator.plot.co", "Chaikin Oscillator"),
                values,
                ink3(
                    text(inputs, "bullColor", BULLISH),
                    text(inputs, "bearColor", BEARISH),
                    text(inputs, "color", NEUTRAL),
                ),
                NEUTRAL,
            )))
        },
    )
    .short_title(msg("indicator.chaikinOscillator.shortTitle", "Chaikin Osc"))
}

fn ease_of_movement() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "ease-of-movement",
        msg("indicator.easeOfMovement.title", "Ease of Movement"),
        false,
        vec![
            length_input(
                14.0,
                "length",
                msg("indicator.easeOfMovement.length", "Length"),
                MAX_LENGTH,
                tip("indicator.easeOfMovement.length.tooltip", "Number of bars in the simple moving average smoothing the 1-bar Ease of Movement values."),
            ),
            int_input(
                "divisor",
                msg("indicator.easeOfMovement.divisor", "Divisor"),
                10000.0,
                1.0,
                1_000_000_000.0,
                tip(
                    "indicator.easeOfMovement.divisor.tooltip",
                    "Volume scaling constant of the box ratio; larger values scale the oscillator up. 10000 is the conventional default.",
                ),
            ),
            color_input(NEUTRAL, msg("indicator.easeOfMovement.color", "EOM Line"), "color", tip("indicator.easeOfMovement.color.tooltip", "Color of the line when it sits exactly at zero.")),
            color_input(
                BULLISH,
                msg("indicator.easeOfMovement.bullColor", "Above Zero"),
                "bullColor",
                tip("indicator.easeOfMovement.bullColor.tooltip", "Color of the line and fill while Ease of Movement is positive."),
            ),
            color_input(
                BEARISH,
                msg("indicator.easeOfMovement.bearColor", "Below Zero"),
                "bearColor",
                tip("indicator.easeOfMovement.bearColor.tooltip", "Color of the line and fill while Ease of Movement is negative."),
            ),
        ],
        |bars, inputs| {
            let div = num(inputs, "divisor", 10000.0);
            let movement = change(&source_values(bars, "HL2"), 1);
            let raw: Vec<f64> = bars
                .iter()
                .zip(&movement)
                .map(|(b, &m)| match b.volume {
                    Some(v) if v.is_finite() && m.is_finite() => {
                        if v == 0.0 {
                            0.0
                        } else {
                            (div * m * (b.high - b.low)) / v
                        }
                    }
                    _ => NAN,
                })
                .collect();
            output(centered_oscillator(CenteredOptions::new(
                "eom",
                msg("indicator.easeOfMovement.plot.eom", "EOM"),
                sma(&raw, len(inputs, "length", 14)),
                ink3(text(inputs, "bullColor", BULLISH), text(inputs, "bearColor", BEARISH), text(inputs, "color", NEUTRAL)),
                NEUTRAL,
            )))
        },
    )
    .short_title(msg("indicator.easeOfMovement.shortTitle", "EOM"))
}

fn force_index() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "force-index",
        msg("indicator.forceIndex.title", "Force Index"),
        false,
        vec![
            length_input(
                13.0,
                "length",
                msg("indicator.forceIndex.length", "Length"),
                MAX_LENGTH,
                tip(
                    "indicator.forceIndex.length.tooltip",
                    "EMA length applied to the raw 1-bar force index, (close − close[1]) × volume. Elder used 13 for the intermediate trend and 2 for short-term timing.",
                ),
            ),
            color_input(BULLISH, msg("indicator.forceIndex.color", "Bullish"), "color", tip("indicator.forceIndex.color.tooltip", "Color of the line while above zero (buying pressure).")),
            color_input(
                BEARISH,
                msg("indicator.forceIndex.bearColor", "Bearish"),
                "bearColor",
                tip("indicator.forceIndex.bearColor.tooltip", "Color of the line while below zero (selling pressure)."),
            ),
        ],
        |bars, inputs| {
            let raw = zip(&change(&closes(bars), 1), &volumes(bars), |d, v| d * v);
            let bull = text(inputs, "color", BULLISH);
            let mut opts = CenteredOptions::new(
                "force",
                msg("indicator.forceIndex.plot.force", "Force Index"),
                ema(&raw, len(inputs, "length", 13)),
                // The reference reads an exact zero as buying pressure, so neutral shares the bull ink.
                ink3(bull.clone(), text(inputs, "bearColor", BEARISH), bull),
                NEUTRAL,
            );
            opts.width = Some(2.0);
            opts.value_transparency = Some(60.0);
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.forceIndex.shortTitle", "FI"))
}

/// The Klinger volume force: volume scaled by how the bar's range compares with the
/// cumulative measurement of its trend leg, signed by the trend.
fn klinger_force(bars: &[Ohlcv]) -> Vec<f64> {
    let mut force = vec![NAN; bars.len()];
    let mut prev_trend = 1.0;
    let mut cm = 0.0;
    let mut prev_dm = 0.0;
    for (i, b) in bars.iter().enumerate() {
        let hlc = b.high + b.low + b.close;
        let prev_hlc = if i > 0 {
            bars[i - 1].high + bars[i - 1].low + bars[i - 1].close
        } else {
            hlc
        };
        let trend = if hlc > prev_hlc { 1.0 } else { -1.0 };
        let dm = b.high - b.low;
        // The cumulative measurement resets to the previous bar's range whenever the trend flips.
        cm = if i > 0 && trend == prev_trend {
            cm + dm
        } else {
            prev_dm + dm
        };
        force[i] = match b.volume {
            Some(v) if v.is_finite() => {
                if cm == 0.0 {
                    0.0
                } else {
                    v * (2.0 * (dm / cm) - 1.0).abs() * trend * 100.0
                }
            }
            _ => NAN,
        };
        prev_trend = trend;
        prev_dm = dm;
    }
    force
}

fn klinger_oscillator() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "klinger-oscillator",
        msg("indicator.klingerOscillator.title", "Klinger Oscillator"),
        false,
        vec![
            length_input(
                34.0,
                "fastLength",
                msg("indicator.klingerOscillator.fastLength", "Fast Length"),
                MAX_LENGTH,
                tip(
                    "indicator.klingerOscillator.fastLength.tooltip",
                    "Length of the fast EMA applied to the volume force.",
                ),
            ),
            length_input(
                55.0,
                "slowLength",
                msg("indicator.klingerOscillator.slowLength", "Slow Length"),
                MAX_LENGTH,
                tip(
                    "indicator.klingerOscillator.slowLength.tooltip",
                    "Length of the slow EMA applied to the volume force.",
                ),
            ),
            length_input(
                13.0,
                "signalLength",
                msg("indicator.klingerOscillator.signalLength", "Signal Length"),
                500.0,
                tip(
                    "indicator.klingerOscillator.signalLength.tooltip",
                    "Length of the EMA applied to the oscillator to obtain the signal line.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.klingerOscillator.color", "Bullish"),
                "color",
                tip(
                    "indicator.klingerOscillator.color.tooltip",
                    "Color of the Klinger oscillator while above zero.",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.klingerOscillator.bearColor", "Bearish"),
                "bearColor",
                tip(
                    "indicator.klingerOscillator.bearColor.tooltip",
                    "Color of the Klinger oscillator while below zero.",
                ),
            ),
            color_input(
                WARNING,
                msg("indicator.klingerOscillator.signalColor", "Signal"),
                "signalColor",
                tip(
                    "indicator.klingerOscillator.signalColor.tooltip",
                    "Color of the signal line.",
                ),
            ),
        ],
        |bars, inputs| {
            let force = klinger_force(bars);
            let line = sub(
                &ema(&force, len(inputs, "fastLength", 34)),
                &ema(&force, len(inputs, "slowLength", 55)),
            );
            let bull = text(inputs, "color", BULLISH);
            let signal = ClassicPlot::new(
                "signal",
                msg("indicator.klingerOscillator.plot.signal", "Signal"),
                ema(&line, len(inputs, "signalLength", 13)),
                text(inputs, "signalColor", WARNING),
            );
            let mut opts = CenteredOptions::new(
                "kvo",
                msg("indicator.klingerOscillator.plot.kvo", "KVO"),
                line,
                ink3(bull.clone(), text(inputs, "bearColor", BEARISH), bull),
                transp(NEUTRAL, 50.0),
            );
            opts.value_transparency = Some(60.0);
            opts.extra_plots = vec![signal];
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.klingerOscillator.shortTitle", "KVO"))
}

fn money_flow_index() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "money-flow-index",
        msg("indicator.moneyFlowIndex.title", "Money Flow Index"),
        false,
        vec![
            length_input(
                14.0,
                "length",
                msg("indicator.moneyFlowIndex.length", "Length"),
                MAX_LENGTH,
                tip("indicator.moneyFlowIndex.length.tooltip", "Number of bars used to sum positive and negative money flow."),
            ),
            float_input(
                "overbought",
                msg("indicator.moneyFlowIndex.overbought", "Overbought Level"),
                80.0,
                0.0,
                100.0,
                1.0,
                tip("indicator.moneyFlowIndex.overbought.tooltip", "Level above which the MFI is considered overbought."),
            ),
            float_input(
                "oversold",
                msg("indicator.moneyFlowIndex.oversold", "Oversold Level"),
                20.0,
                0.0,
                100.0,
                1.0,
                tip("indicator.moneyFlowIndex.oversold.tooltip", "Level below which the MFI is considered oversold."),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.moneyFlowIndex.color", "MFI"),
                "color",
                tip("indicator.moneyFlowIndex.color.tooltip", "Color of the MFI line between the levels. Toward either level it blends into that level’s color."),
            ),
            color_input(
                BEARISH,
                msg("indicator.moneyFlowIndex.overboughtColor", "Overbought"),
                "overboughtColor",
                tip("indicator.moneyFlowIndex.overboughtColor.tooltip", "Color of the line and zone shading at and above the overbought level."),
            ),
            color_input(
                BULLISH,
                msg("indicator.moneyFlowIndex.oversoldColor", "Oversold"),
                "oversoldColor",
                tip("indicator.moneyFlowIndex.oversoldColor.tooltip", "Color of the line and zone shading at and below the oversold level."),
            ),
            color_input(
                &transp(NEUTRAL, 94.0),
                msg("indicator.moneyFlowIndex.fillColor", "OB/OS Zone Fill"),
                "fillColor",
                tip("indicator.moneyFlowIndex.fillColor.tooltip", "Fill color of the area between the overbought and oversold levels."),
            ),
        ],
        |bars, inputs| {
            let n = len(inputs, "length", 14);
            let tp = source_values(bars, "HLC3");
            let raw_flow = zip(&tp, &volumes(bars), |p, v| p * v);
            let d = change(&tp, 1);
            let pos = sum(&zip(&raw_flow, &d, |f, x| if x > 0.0 { f } else { 0.0 }), n);
            let neg = sum(&zip(&raw_flow, &d, |f, x| if x < 0.0 { f } else { 0.0 }), n);
            let values = zip(&pos, &neg, |p, w| if p + w > 0.0 { (100.0 * p) / (p + w) } else { NAN });
            output(bounded_oscillator(BoundedOptions {
                key: "mfi".into(),
                title: msg("indicator.moneyFlowIndex.plot.mfi", "MFI"),
                values,
                overbought: num(inputs, "overbought", 80.0),
                oversold: num(inputs, "oversold", 20.0),
                scale_top: 100.0,
                scale_bottom: 0.0,
                ink: RampInk {
                    low: text(inputs, "oversoldColor", BULLISH),
                    mid: text(inputs, "color", NEUTRAL),
                    high: text(inputs, "overboughtColor", BEARISH),
                },
                level_ink: NEUTRAL.into(),
                width: None,
                zone_transparency: Some(91.0),
                midline: None,
                band_fill: Some(text(inputs, "fillColor", &transp(NEUTRAL, 94.0))),
                midpoint_wash: false,
                level_titles: None,
            }))
        },
    )
    .short_title(msg("indicator.moneyFlowIndex.shortTitle", "MFI"))
}

/// The style inputs NVI and PVI share.
fn regime_style_inputs(signal_title: MessageLike) -> Vec<ClassicInput> {
    vec![
        color_input(
            BULLISH,
            msg("indicator.common.bullColor", "Bullish"),
            "bullColor",
            tip(
                "indicator.regime.bullColor.tooltip",
                "Color of the line and gradient fill while it holds above its signal EMA.",
            ),
        ),
        color_input(
            BEARISH,
            msg("indicator.common.bearColor", "Bearish"),
            "bearColor",
            tip(
                "indicator.regime.bearColor.tooltip",
                "Color of the line and gradient fill while it holds below its signal EMA.",
            ),
        ),
        color_input(
            NEUTRAL,
            msg("indicator.common.neutralColor", "Neutral"),
            "neutralColor",
            tip(
                "indicator.regime.neutralColor.tooltip",
                "Color used while the signal EMA is still forming, or the line sits exactly on it.",
            ),
        ),
        color_input(
            NEUTRAL,
            signal_title,
            "signalColor",
            tip(
                "indicator.regime.signalColor.tooltip",
                "Color of the signal EMA overlay.",
            ),
        ),
        bool_input(
            "gradientFill",
            msg("indicator.regime.gradientFill", "Gradient Fill"),
            true,
            STYLE,
            tip(
                "indicator.regime.gradientFill.tooltip",
                "Fills the gap between the line and its signal EMA with a vertical gradient — transparent at the average, strongest at the line.",
            ),
        ),
    ]
}

fn regime_ink(inputs: &crate::model::Inputs) -> RegimeInk {
    RegimeInk {
        bull: text(inputs, "bullColor", BULLISH),
        bear: text(inputs, "bearColor", BEARISH),
        neutral: text(inputs, "neutralColor", NEUTRAL),
        signal: text(inputs, "signalColor", NEUTRAL),
    }
}

const NVI_POSITION: &str = "Position vs Average";
const NVI_SINGLE: &str = "Single Color";

fn negative_volume_index() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        float_input(
            "base",
            msg("indicator.negativeVolumeIndex.base", "Starting Value"),
            1000.0,
            0.01,
            1_000_000.0,
            1.0,
            tip(
                "indicator.negativeVolumeIndex.base.tooltip",
                "Arbitrary seed of the cumulative line, conventionally 1000. The absolute level carries no information — the line is read by slope and by position against the signal EMA.",
            ),
        ),
        bool_input(
            "showSignal",
            msg("indicator.negativeVolumeIndex.showSignal", "Signal EMA"),
            true,
            SETTINGS,
            tip(
                "indicator.negativeVolumeIndex.showSignal.tooltip",
                "Displays the conventional signal overlay. Hiding it only removes the plotted average; the regime coloring and fill still use it.",
            ),
        ),
        length_input(
            255.0,
            "signalLength",
            msg(
                "indicator.negativeVolumeIndex.signalLength",
                "Signal Length",
            ),
            MAX_LENGTH,
            tip(
                "indicator.negativeVolumeIndex.signalLength.tooltip",
                "Length of the signal EMA. The convention is 255 sessions on daily charts, about one trading year.",
            ),
        ),
        option_input(
            "colorMode",
            msg("indicator.negativeVolumeIndex.colorMode", "Line Coloring"),
            NVI_POSITION,
            &[NVI_POSITION, NVI_SINGLE],
            tip(
                "indicator.negativeVolumeIndex.colorMode.tooltip",
                "Position vs Average colors the line by the classic regime read; Single Color draws the whole line in the neutral color.",
            ),
        ),
    ];
    inputs.extend(regime_style_inputs(msg(
        "indicator.negativeVolumeIndex.signalColor",
        "Signal EMA",
    )));
    ClassicIndicatorSpec::new(
        "negative-volume-index",
        msg(
            "indicator.negativeVolumeIndex.title",
            "Negative Volume Index",
        ),
        false,
        inputs,
        |bars, inputs| {
            let values = volume_index(bars, &closes(bars), num(inputs, "base", 1000.0), |v, pv| {
                v < pv
            });
            let signal = ema(&values, len(inputs, "signalLength", 255));
            output(regime_index(
                "nvi",
                "NVI",
                values,
                signal,
                &regime_ink(inputs),
                flag(inputs, "showSignal", true),
                flag(inputs, "gradientFill", true),
                text(inputs, "colorMode", NVI_POSITION) == NVI_SINGLE,
            ))
        },
    )
    .short_title(msg(
        "indicator.negativeVolumeIndex.shortTitle",
        "Negative Volume Index",
    ))
}

fn positive_volume_index() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        source_input(
            "Close",
            "source",
            msg("indicator.positiveVolumeIndex.source", "Source"),
            tip(
                "indicator.positiveVolumeIndex.source.tooltip",
                "Price series whose one-bar percentage change moves PVI on its active sessions.",
            ),
        ),
        float_input(
            "base",
            msg("indicator.positiveVolumeIndex.base", "Starting Value"),
            1000.0,
            1.0,
            1_000_000.0,
            1.0,
            tip(
                "indicator.positiveVolumeIndex.base.tooltip",
                "Seed of the cumulative line. The absolute level carries no information, so the reads are the slope and the position against the signal EMA.",
            ),
        ),
        bool_input(
            "showSignal",
            msg("indicator.positiveVolumeIndex.showSignal", "Signal EMA"),
            true,
            SETTINGS,
            tip(
                "indicator.positiveVolumeIndex.showSignal.tooltip",
                "Plots the long EMA of PVI that anchors the classic regime read. Hiding it keeps the regime coloring and fill, which are always measured against it.",
            ),
        ),
        length_input(
            255.0,
            "signalLength",
            msg(
                "indicator.positiveVolumeIndex.signalLength",
                "Signal Length",
            ),
            MAX_LENGTH,
            tip(
                "indicator.positiveVolumeIndex.signalLength.tooltip",
                "Length of the signal EMA. The classic reference is 255 trading days on daily charts, about one year.",
            ),
        ),
    ];
    inputs.extend(regime_style_inputs(msg(
        "indicator.positiveVolumeIndex.signalColor",
        "Signal Line",
    )));
    ClassicIndicatorSpec::new(
        "positive-volume-index",
        msg(
            "indicator.positiveVolumeIndex.title",
            "Positive Volume Index",
        ),
        false,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let values = volume_index(bars, &src, num(inputs, "base", 1000.0), |v, pv| v > pv);
            let signal = ema(&values, len(inputs, "signalLength", 255));
            output(regime_index(
                "pvi",
                "PVI",
                values,
                signal,
                &regime_ink(inputs),
                flag(inputs, "showSignal", true),
                flag(inputs, "gradientFill", true),
                false,
            ))
        },
    )
    .short_title(msg(
        "indicator.positiveVolumeIndex.shortTitle",
        "Positive Volume Index",
    ))
}

fn price_volume_trend() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "price-volume-trend",
        msg("indicator.priceVolumeTrend.title", "Price Volume Trend"),
        false,
        vec![
            bool_input(
                "showSignal",
                msg("indicator.priceVolumeTrend.showSignal", "Signal Line"),
                false,
                SETTINGS,
                tip(
                    "indicator.priceVolumeTrend.showSignal.tooltip",
                    "Display a moving average of PVT acting as a signal line.",
                ),
            ),
            option_input(
                "signalType",
                msg("indicator.priceVolumeTrend.signalType", "Signal Type"),
                "SMA",
                &["SMA", "EMA"],
                tip(
                    "indicator.priceVolumeTrend.signalType.tooltip",
                    "Moving average type used for the signal line.",
                ),
            ),
            length_input(
                21.0,
                "signalLength",
                msg("indicator.priceVolumeTrend.signalLength", "Signal Length"),
                MAX_LENGTH,
                tip(
                    "indicator.priceVolumeTrend.signalLength.tooltip",
                    "Number of bars used to compute the signal line.",
                ),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.priceVolumeTrend.color", "PVT Color"),
                "color",
                tip(
                    "indicator.priceVolumeTrend.color.tooltip",
                    "Color of the PVT line while it is flat.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.priceVolumeTrend.bullColor", "Rising"),
                "bullColor",
                tip(
                    "indicator.priceVolumeTrend.bullColor.tooltip",
                    "Color of the PVT line while it rises bar to bar.",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.priceVolumeTrend.bearColor", "Falling"),
                "bearColor",
                tip(
                    "indicator.priceVolumeTrend.bearColor.tooltip",
                    "Color of the PVT line while it falls bar to bar.",
                ),
            ),
            color_input(
                WARNING,
                msg("indicator.priceVolumeTrend.signalColor", "Signal Color"),
                "signalColor",
                tip(
                    "indicator.priceVolumeTrend.signalColor.tooltip",
                    "Color of the signal line.",
                ),
            ),
        ],
        |bars, inputs| {
            let c = closes(bars);
            let raw: Vec<f64> = bars
                .iter()
                .enumerate()
                .map(|(i, b)| match b.volume {
                    Some(v) if i > 0 && v.is_finite() => {
                        let pc = c[i - 1];
                        if pc == 0.0 {
                            0.0
                        } else {
                            ((c[i] - pc) / pc) * v
                        }
                    }
                    _ => NAN,
                })
                .collect();
            let values = cum_sum(&raw);
            let flat = text(inputs, "color", NEUTRAL);
            let colors = slope_colors(
                &values,
                &text(inputs, "bullColor", BULLISH),
                &text(inputs, "bearColor", BEARISH),
                &flat,
            );
            let mut plots = vec![
                ClassicPlot::new(
                    "pvt",
                    msg("indicator.priceVolumeTrend.plot.pvt", "PVT"),
                    values.clone(),
                    flat,
                )
                .width(2.0)
                .colors(colors),
            ];
            if flag(inputs, "showSignal", false) {
                let n = len(inputs, "signalLength", 21);
                let signal = if text(inputs, "signalType", "SMA") == "EMA" {
                    ema(&values, n)
                } else {
                    sma(&values, n)
                };
                plots.push(ClassicPlot::new(
                    "signal",
                    msg("indicator.priceVolumeTrend.plot.signal", "Signal"),
                    signal,
                    text(inputs, "signalColor", WARNING),
                ));
            }
            ClassicOutput {
                plots,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.priceVolumeTrend.shortTitle", "PVT"))
}

fn volume_oscillator() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "volume-oscillator",
        msg("indicator.volumeOscillator.title", "Volume Oscillator"),
        false,
        vec![
            length_input(
                5.0,
                "fastLength",
                msg("indicator.volumeOscillator.fastLength", "Fast Length"),
                MAX_LENGTH,
                tip(
                    "indicator.volumeOscillator.fastLength.tooltip",
                    "Length of the fast EMA of volume.",
                ),
            ),
            length_input(
                10.0,
                "slowLength",
                msg("indicator.volumeOscillator.slowLength", "Slow Length"),
                MAX_LENGTH,
                tip(
                    "indicator.volumeOscillator.slowLength.tooltip",
                    "Length of the slow EMA of volume.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.volumeOscillator.color", "Above Zero"),
                "color",
                tip(
                    "indicator.volumeOscillator.color.tooltip",
                    "Color of the line and fill while it holds above zero — volume expanding.",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.volumeOscillator.bearColor", "Below Zero"),
                "bearColor",
                tip(
                    "indicator.volumeOscillator.bearColor.tooltip",
                    "Color of the line and fill while it holds below zero — volume contracting.",
                ),
            ),
        ],
        |bars, inputs| {
            let vol = volumes(bars);
            let fast = ema(&vol, len(inputs, "fastLength", 5));
            let slow = ema(&vol, len(inputs, "slowLength", 10));
            let values = zip(&fast, &slow, |f, s| {
                if s == 0.0 { 0.0 } else { (100.0 * (f - s)) / s }
            });
            let bull = text(inputs, "color", BULLISH);
            let mut opts = CenteredOptions::new(
                "vo",
                msg("indicator.volumeOscillator.plot.vo", "Volume Oscillator"),
                values,
                ink3(bull.clone(), text(inputs, "bearColor", BEARISH), bull),
                NEUTRAL,
            );
            opts.value_transparency = Some(60.0);
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.volumeOscillator.shortTitle", "Vol Osc"))
}

const OSCILLATOR: &str = "Oscillator";

fn pvo() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "pvo",
        msg("indicator.pvo.title", "Percentage Volume Oscillator"),
        false,
        vec![
            length_input(12.0, "fastLength", msg("indicator.pvo.fastLength", "Fast Length"), MAX_LENGTH, tip("indicator.pvo.fastLength.tooltip", "Period of the fast EMA of volume. The conventional setting is 12."))
                .group(OSCILLATOR),
            length_input(
                26.0,
                "slowLength",
                msg("indicator.pvo.slowLength", "Slow Length"),
                MAX_LENGTH,
                tip(
                    "indicator.pvo.slowLength.tooltip",
                    "Period of the slow EMA of volume. The PVO expresses the fast/slow spread as a percentage of this slow average.",
                ),
            )
            .group(OSCILLATOR),
            length_input(
                9.0,
                "signalLength",
                msg("indicator.pvo.signalLength", "Signal Length"),
                500.0,
                tip("indicator.pvo.signalLength.tooltip", "Period of the EMA applied to the PVO to form the signal line."),
            )
            .group(OSCILLATOR),
            color_input(
                BULLISH,
                msg("indicator.pvo.color", "Expansion Color"),
                "color",
                tip("indicator.pvo.color.tooltip", "Color of the oscillator and fill while participation expands."),
            ),
            color_input(
                BEARISH,
                msg("indicator.pvo.bearColor", "Contraction Color"),
                "bearColor",
                tip("indicator.pvo.bearColor.tooltip", "Color of the oscillator and fill while participation contracts."),
            ),
            color_input(WARNING, msg("indicator.pvo.signalColor", "Signal Line"), "signalColor", tip("indicator.pvo.signalColor.tooltip", "Color of the signal line.")),
            color_input(
                &transp(NEUTRAL, 50.0),
                msg("indicator.pvo.zeroColor", "Zero Line"),
                "zeroColor",
                tip("indicator.pvo.zeroColor.tooltip", "Color of the zero reference line."),
            ),
            bool_input("showSignal", msg("indicator.pvo.showSignal", "Show Signal Line"), true, STYLE, tip("indicator.pvo.showSignal.tooltip", "Plot the signal line, an EMA of the PVO.")),
            bool_input(
                "showHistogram",
                msg("indicator.pvo.showHistogram", "Show Histogram"),
                true,
                STYLE,
                tip("indicator.pvo.showHistogram.tooltip", "Plot the difference between the PVO and its signal line as columns behind the oscillator."),
            ),
        ],
        |bars, inputs| {
            let vol = volumes(bars);
            let fast = ema(&vol, len(inputs, "fastLength", 12));
            let slow = ema(&vol, len(inputs, "slowLength", 26));
            let values = zip(&fast, &slow, |f, s| if s > 0.0 { (100.0 * (f - s)) / s } else { NAN });
            let signal = ema(&values, len(inputs, "signalLength", 9));
            let expansion = text(inputs, "color", BULLISH);
            let contraction = text(inputs, "bearColor", BEARISH);
            let mut extra_plots = Vec::new();
            if flag(inputs, "showHistogram", true) {
                let hist = sub(&values, &signal);
                let colors: Vec<Option<String>> = hist.iter().map(|&x| x.is_finite().then(|| transp(if x > 0.0 { &expansion } else { &contraction }, 60.0))).collect();
                extra_plots.push(
                    ClassicPlot::new("hist", msg("indicator.pvo.plot.hist", "Histogram"), hist, transp(&expansion, 60.0)).kind(LineLikeKind::Columns).colors(colors).base(0.0),
                );
            }
            if flag(inputs, "showSignal", true) {
                extra_plots.push(ClassicPlot::new("signal", msg("indicator.pvo.plot.signal", "Signal"), signal, text(inputs, "signalColor", WARNING)));
            }
            let mut opts = CenteredOptions::new(
                "pvo",
                msg("indicator.pvo.plot.pvo", "PVO"),
                values,
                ink3(expansion, contraction.clone(), contraction),
                text(inputs, "zeroColor", &transp(NEUTRAL, 50.0)),
            );
            opts.extra_plots = extra_plots;
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.pvo.shortTitle", "PVO"))
}

fn volume_flow_indicator() -> ClassicIndicatorSpec {
    let mut length = length_input(
        130.0,
        "length",
        msg("indicator.volumeFlowIndicator.length", "Length"),
        MAX_LENGTH,
        tip(
            "indicator.volumeFlowIndicator.length.tooltip",
            "Rolling summation window, also used for the average volume that scales the result. The published default of 130 bars targets position-horizon work on daily charts.",
        ),
    );
    length.min = Some(2.0);
    let mut vol_length = length_input(
        30.0,
        "volLength",
        msg(
            "indicator.volumeFlowIndicator.volLength",
            "Volatility Length",
        ),
        MAX_LENGTH,
        tip(
            "indicator.volumeFlowIndicator.volLength.tooltip",
            "Window of the standard deviation of the log change in typical price used to build the adaptive cutoff.",
        ),
    );
    vol_length.min = Some(2.0);
    ClassicIndicatorSpec::new(
        "volume-flow-indicator",
        msg("indicator.volumeFlowIndicator.title", "Volume Flow Indicator"),
        false,
        vec![
            length,
            float_input(
                "coef",
                msg("indicator.volumeFlowIndicator.coef", "Cutoff Coefficient"),
                0.2,
                0.0,
                10.0,
                0.05,
                tip(
                    "indicator.volumeFlowIndicator.coef.tooltip",
                    "Scales the volatility-based minimum change in typical price. Moves smaller than the cutoff count as noise and contribute no volume.",
                ),
            ),
            float_input(
                "volCoef",
                msg("indicator.volumeFlowIndicator.volCoef", "Volume Cap Multiplier"),
                2.5,
                0.1,
                50.0,
                0.1,
                tip(
                    "indicator.volumeFlowIndicator.volCoef.tooltip",
                    "Caps each bar's volume contribution at this multiple of the average volume, so a single climactic print stays bounded.",
                ),
            ),
            vol_length,
            bool_input(
                "smooth",
                msg("indicator.volumeFlowIndicator.smooth", "Smooth VFI"),
                true,
                SETTINGS,
                tip("indicator.volumeFlowIndicator.smooth.tooltip", "Applies the published light smoothing — an EMA of the summed line — before plotting."),
            ),
            length_input(
                3.0,
                "smoothLength",
                msg("indicator.volumeFlowIndicator.smoothLength", "Smoothing Length"),
                500.0,
                tip("indicator.volumeFlowIndicator.smoothLength.tooltip", "Length of the smoothing EMA. The published version uses 3 bars."),
            ),
            bool_input(
                "showSignal",
                msg("indicator.volumeFlowIndicator.showSignal", "Signal Line"),
                true,
                SETTINGS,
                tip("indicator.volumeFlowIndicator.showSignal.tooltip", "Plots an EMA of VFI as a signal line."),
            ),
            length_input(
                5.0,
                "signalLength",
                msg("indicator.volumeFlowIndicator.signalLength", "Signal Length"),
                500.0,
                tip("indicator.volumeFlowIndicator.signalLength.tooltip", "Length of the signal-line EMA."),
            ),
            color_input(
                BULLISH,
                msg("indicator.volumeFlowIndicator.color", "Bullish"),
                "color",
                tip("indicator.volumeFlowIndicator.color.tooltip", "Color of the VFI line and gradient fill above the zero line — the net accumulation regime."),
            ),
            color_input(
                BEARISH,
                msg("indicator.volumeFlowIndicator.bearColor", "Bearish"),
                "bearColor",
                tip("indicator.volumeFlowIndicator.bearColor.tooltip", "Color of the VFI line and gradient fill below the zero line — the net distribution regime."),
            ),
            color_input(
                WARNING,
                msg("indicator.volumeFlowIndicator.signalColor", "Signal Line"),
                "signalColor",
                tip("indicator.volumeFlowIndicator.signalColor.tooltip", "Color of the signal line."),
            ),
            bool_input(
                "gradientFill",
                msg("indicator.volumeFlowIndicator.gradientFill", "Gradient Fill"),
                true,
                STYLE,
                tip("indicator.volumeFlowIndicator.gradientFill.tooltip", "Fills the area between the zero line and the VFI line with a vertical gradient."),
            ),
        ],
        |bars, inputs| {
            let n = len(inputs, "length", 130);
            let typical = source_values(bars, "HLC3");
            let log_typical = map(&typical, f64::ln);
            let log_change = sub(&log_typical, &shift(&log_typical, 1));
            let volatility = stdev(&log_change, len(inputs, "volLength", 30));
            let coef = num(inputs, "coef", 0.2);
            let vol_coef = num(inputs, "volCoef", 2.5);
            let vol = volumes(bars);
            // The cap reads the PREVIOUS bar's average, so the current bar never scales itself.
            let avg_volume = shift(&sma(&vol, n), 1);
            let typical_change = change(&typical, 1);
            let signed: Vec<f64> = (0..bars.len())
                .map(|i| {
                    let (sd, m, v) = (volatility[i], typical_change[i], vol[i]);
                    if !sd.is_finite() || !m.is_finite() || !v.is_finite() {
                        return NAN;
                    }
                    let cutoff = coef * sd * bars[i].close;
                    let avg = avg_volume[i];
                    let capped = if avg.is_finite() { js_min(v, avg * vol_coef) } else { v };
                    if m > cutoff {
                        capped
                    } else if m < -cutoff {
                        -capped
                    } else {
                        0.0
                    }
                })
                .collect();
            let raw = zip(&sum(&signed, n), &avg_volume, |f, a| if a > 0.0 { f / a } else { NAN });
            let values = if flag(inputs, "smooth", true) { ema(&raw, len(inputs, "smoothLength", 3)) } else { raw };
            let bull = text(inputs, "color", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let extra_plots = if flag(inputs, "showSignal", true) {
                vec![ClassicPlot::new(
                    "signal",
                    msg("indicator.volumeFlowIndicator.plot.signal", "Signal Line"),
                    ema(&values, len(inputs, "signalLength", 5)),
                    text(inputs, "signalColor", WARNING),
                )]
            } else {
                Vec::new()
            };
            let mut opts = CenteredOptions::new("vfi", msg("indicator.volumeFlowIndicator.plot.vfi", "VFI"), values, ink3(bull, bear.clone(), bear), transp(NEUTRAL, 90.0));
            opts.fill = flag(inputs, "gradientFill", true);
            opts.extra_plots = extra_plots;
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.volumeFlowIndicator.shortTitle", "Volume Flow Indicator"))
}

const II_OSCILLATOR: &str = "Normalized Oscillator (II%)";
const II_CUMULATIVE: &str = "Cumulative Line";

fn intraday_intensity() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "intraday-intensity",
        msg("indicator.intradayIntensity.title", "Intraday Intensity"),
        false,
        vec![
            option_input(
                "mode",
                msg("indicator.intradayIntensity.mode", "Display"),
                II_OSCILLATOR,
                &[II_OSCILLATOR, II_CUMULATIVE],
                tip(
                    "indicator.intradayIntensity.mode.tooltip",
                    "The two published forms. The cumulative line is open-ended and read by slope against price; the normalized oscillator divides a rolling sum of intensity by the rolling sum of volume and oscillates around zero.",
                ),
            ),
            length_input(
                21.0,
                "length",
                msg("indicator.intradayIntensity.length", "Oscillator Length"),
                MAX_LENGTH,
                tip(
                    "indicator.intradayIntensity.length.tooltip",
                    "Window of the rolling sums in the normalized form. The common setting is 21 bars. It has no effect on the cumulative line.",
                ),
            ),
            color_input(
                BULLISH,
                msg("indicator.intradayIntensity.color", "Bullish"),
                "color",
                tip(
                    "indicator.intradayIntensity.color.tooltip",
                    "Color of positive readings, the rising cumulative line and the fill above zero — the accumulation side.",
                ),
            ),
            color_input(
                BEARISH,
                msg("indicator.intradayIntensity.bearColor", "Bearish"),
                "bearColor",
                tip(
                    "indicator.intradayIntensity.bearColor.tooltip",
                    "Color of negative readings, the falling cumulative line and the fill below zero — the distribution side.",
                ),
            ),
            bool_input(
                "gradientFill",
                msg("indicator.intradayIntensity.gradientFill", "Gradient Fill"),
                true,
                STYLE,
                tip(
                    "indicator.intradayIntensity.gradientFill.tooltip",
                    "Fills the area between the zero line and the oscillator with a vertical gradient. Applies to the normalized oscillator only.",
                ),
            ),
        ],
        |bars, inputs| {
            let intensity = money_flow_volume(bars);
            let bull = text(inputs, "color", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            if text(inputs, "mode", II_OSCILLATOR) == II_CUMULATIVE {
                // The open-ended line is read by slope, so it carries no zero line and no wash.
                let values = cum_sum(&intensity);
                let colors = slope_colors(&values, &bull, &bear, &bull);
                let plot = ClassicPlot::new("ii", msg("indicator.intradayIntensity.plot.ii", "Intraday Intensity"), values, bull).colors(colors);
                return ClassicOutput { plots: vec![plot], ..Default::default() };
            }
            let n = len(inputs, "length", 21);
            let values = zip(&sum(&intensity, n), &sum(&volumes(bars), n), |m, v| if v > 0.0 { (100.0 * m) / v } else { NAN });
            let mut opts = CenteredOptions::new(
                "ii",
                msg("indicator.intradayIntensity.plot.ii", "Intraday Intensity"),
                values,
                ink3(bull, bear.clone(), bear),
                transp(NEUTRAL, 90.0),
            );
            opts.fill = flag(inputs, "gradientFill", true);
            output(centered_oscillator(opts))
        },
    )
    .short_title(msg("indicator.intradayIntensity.shortTitle", "Intraday Intensity"))
}

pub fn volume_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        on_balance_volume(),
        accumulation_distribution(),
        chaikin_money_flow(),
        chaikin_oscillator(),
        ease_of_movement(),
        force_index(),
        klinger_oscillator(),
        money_flow_index(),
        negative_volume_index(),
        positive_volume_index(),
        price_volume_trend(),
        volume_oscillator(),
        pvo(),
        volume_flow_indicator(),
        intraday_intensity(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::classics::define::classic_native_output;
    use crate::model::{Inputs, SeriesData};

    fn synthetic_bars(n: usize, with_volume: bool) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| {
                let t = i as f64;
                let close = 100.0 + 10.0 * (t / 9.0).sin() + t * 0.05;
                let open = close - 0.4 * (t / 3.0).cos();
                let high = close.max(open) + 1.0 + (t % 4.0) * 0.2;
                let low = close.min(open) - 1.0 - (t % 3.0) * 0.2;
                let volume =
                    with_volume.then(|| 1000.0 + 500.0 * (t / 5.0).sin() + (t % 7.0) * 40.0);
                Ohlcv::new(i as i64 * 60_000, open, high, low, close, volume)
            })
            .collect()
    }

    fn spec_of(kind: &str) -> ClassicIndicatorSpec {
        volume_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    fn flat_bar(i: i64, close: f64, volume: f64) -> Ohlcv {
        Ohlcv::new(
            i * 60_000,
            close,
            close + 1.0,
            close - 1.0,
            close,
            Some(volume),
        )
    }

    fn plot_values(spec: &ClassicIndicatorSpec, bars: &[Ohlcv], key: &str) -> Vec<f64> {
        let out = spec.compute(bars, &spec.default_inputs());
        out.plots
            .into_iter()
            .find(|p| p.key == key)
            .expect("plot exists")
            .values
    }

    #[test]
    fn catalog_keeps_the_original_order() {
        let types: Vec<&str> = volume_specs().iter().map(|s| s.indicator_type).collect();
        assert_eq!(
            types,
            vec![
                "on-balance-volume",
                "accumulation-distribution",
                "chaikin-money-flow",
                "chaikin-oscillator",
                "ease-of-movement",
                "force-index",
                "klinger-oscillator",
                "money-flow-index",
                "negative-volume-index",
                "positive-volume-index",
                "price-volume-trend",
                "volume-oscillator",
                "pvo",
                "volume-flow-indicator",
                "intraday-intensity",
            ]
        );
    }

    #[test]
    fn every_spec_computes_bar_aligned_plots() {
        let bars = synthetic_bars(400, true);
        for spec in volume_specs() {
            let out = spec.compute(&bars, &spec.default_inputs());
            assert!(!out.plots.is_empty(), "{} has plots", spec.indicator_type);
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    bars.len(),
                    "{} / {}",
                    spec.indicator_type,
                    plot.key
                );
            }
            if let Some(colors) = out.plots.iter().filter_map(|p| p.colors.as_ref()).next() {
                assert_eq!(colors.len(), bars.len());
            }
        }
    }

    #[test]
    fn every_spec_keeps_a_visible_pane_plot() {
        let bars = synthetic_bars(400, true);
        for spec in volume_specs() {
            assert!(!spec.overlay);
            let out = spec.compute(&bars, &spec.default_inputs());
            let visible = out
                .plots
                .iter()
                .any(|p| !p.overlay && p.display.is_none_or(|d| d.pane != Some(false)));
            assert!(visible, "{} keeps a pane plot", spec.indicator_type);
        }
    }

    #[test]
    fn every_spec_has_a_short_title_and_default_for_each_input() {
        for spec in volume_specs() {
            assert!(spec.short_title.is_some(), "{}", spec.indicator_type);
            assert_eq!(spec.default_inputs().len(), spec.inputs.len());
        }
    }

    #[test]
    fn chaikin_money_flow_stays_a_gap_without_volume() {
        let spec = spec_of("chaikin-money-flow");
        let bars = synthetic_bars(60, false);
        let out = classic_native_output(&spec, &bars, &spec.default_inputs());
        let SeriesData::LineLike { points, .. } = &out.series[0].data else {
            panic!("line series")
        };
        assert!(points.iter().all(|p| p.value.is_none()));
    }

    #[test]
    fn volume_studies_stay_gaps_without_volume() {
        let bars = synthetic_bars(60, false);
        for kind in [
            "on-balance-volume",
            "accumulation-distribution",
            "chaikin-oscillator",
            "ease-of-movement",
            "force-index",
            "klinger-oscillator",
            "money-flow-index",
            "price-volume-trend",
        ] {
            let spec = spec_of(kind);
            let out = spec.compute(&bars, &spec.default_inputs());
            assert!(out.plots[0].values.iter().all(|v| v.is_nan()), "{kind}");
        }
    }

    #[test]
    fn on_balance_volume_adds_and_subtracts_volume_by_close_direction() {
        let closes = [10.0, 11.0, 10.0, 10.0, 12.0];
        let volumes = [100.0, 200.0, 300.0, 400.0, 500.0];
        let bars: Vec<Ohlcv> = closes
            .iter()
            .zip(volumes)
            .enumerate()
            .map(|(i, (&c, v))| flat_bar(i as i64, c, v))
            .collect();
        let obv = plot_values(&spec_of("on-balance-volume"), &bars, "obv");
        assert!(obv[0].is_nan());
        assert_eq!(&obv[1..], &[200.0, -100.0, -100.0, 400.0]);
    }

    #[test]
    fn accumulation_distribution_sums_close_location_volume() {
        // Close at the high of a 8..12 range: +1 x volume. Close at the midpoint: 0. Close at the low: -1.
        let bars = vec![
            Ohlcv::new(0, 10.0, 12.0, 8.0, 12.0, Some(100.0)),
            Ohlcv::new(60_000, 10.0, 12.0, 8.0, 10.0, Some(300.0)),
            Ohlcv::new(120_000, 10.0, 12.0, 8.0, 8.0, Some(40.0)),
            Ohlcv::new(180_000, 10.0, 10.0, 10.0, 10.0, Some(999.0)),
        ];
        let ad = plot_values(&spec_of("accumulation-distribution"), &bars, "ad");
        assert_eq!(ad, vec![100.0, 100.0, 60.0, 60.0]);
    }

    #[test]
    fn accumulation_distribution_colors_follow_the_slope() {
        let bars = vec![
            Ohlcv::new(0, 10.0, 12.0, 8.0, 12.0, Some(100.0)),
            Ohlcv::new(60_000, 10.0, 12.0, 8.0, 12.0, Some(100.0)),
            Ohlcv::new(120_000, 10.0, 12.0, 8.0, 8.0, Some(150.0)),
            Ohlcv::new(180_000, 10.0, 10.0, 10.0, 10.0, Some(150.0)),
        ];
        let spec = spec_of("accumulation-distribution");
        let out = spec.compute(&bars, &spec.default_inputs());
        let colors: Vec<Option<&str>> = out.plots[0]
            .colors
            .as_ref()
            .expect("slope colors")
            .iter()
            .map(|c| c.as_deref())
            .collect();
        assert_eq!(
            colors,
            vec![Some(NEUTRAL), Some(BULLISH), Some(BEARISH), Some(NEUTRAL)]
        );
    }

    #[test]
    fn force_index_with_length_one_is_the_raw_change_times_volume() {
        let spec = spec_of("force-index");
        let mut inputs: Inputs = spec.default_inputs();
        inputs.insert("length".into(), 1.0.into());
        let bars: Vec<Ohlcv> = [(10.0, 100.0), (12.0, 50.0), (11.0, 200.0)]
            .iter()
            .enumerate()
            .map(|(i, &(c, v))| flat_bar(i as i64, c, v))
            .collect();
        let out = spec.compute(&bars, &inputs);
        let force = &out.plots[0].values;
        assert!(force[0].is_nan());
        assert_eq!(&force[1..], &[100.0, -200.0]);
    }

    #[test]
    fn price_volume_trend_scales_the_percent_change_by_volume() {
        let bars: Vec<Ohlcv> = [(100.0, 10.0), (110.0, 20.0), (99.0, 30.0)]
            .iter()
            .enumerate()
            .map(|(i, &(c, v))| flat_bar(i as i64, c, v))
            .collect();
        let pvt = plot_values(&spec_of("price-volume-trend"), &bars, "pvt");
        assert!(pvt[0].is_nan());
        assert!((pvt[1] - 2.0).abs() < 1e-12);
        assert!((pvt[2] - (2.0 - 3.0)).abs() < 1e-12);
    }

    #[test]
    fn negative_volume_index_moves_only_on_falling_volume() {
        let spec = spec_of("negative-volume-index");
        let bars: Vec<Ohlcv> = [(100.0, 10.0), (110.0, 8.0), (121.0, 9.0), (110.0, 5.0)]
            .iter()
            .enumerate()
            .map(|(i, &(c, v))| flat_bar(i as i64, c, v))
            .collect();
        let nvi = plot_values(&spec, &bars, "nvi");
        assert_eq!(nvi[0], 1000.0);
        assert!((nvi[1] - 1100.0).abs() < 1e-9);
        assert!((nvi[2] - 1100.0).abs() < 1e-9);
        assert!((nvi[3] - 1100.0 * (1.0 + (110.0 - 121.0) / 121.0)).abs() < 1e-9);
    }

    #[test]
    fn positive_volume_index_moves_only_on_rising_volume() {
        let spec = spec_of("positive-volume-index");
        let bars: Vec<Ohlcv> = [(100.0, 10.0), (110.0, 8.0), (121.0, 9.0)]
            .iter()
            .enumerate()
            .map(|(i, &(c, v))| flat_bar(i as i64, c, v))
            .collect();
        let pvi = plot_values(&spec, &bars, "pvi");
        assert_eq!(pvi[0], 1000.0);
        assert_eq!(pvi[1], 1000.0);
        assert!((pvi[2] - 1100.0).abs() < 1e-9);
    }

    #[test]
    fn regime_signal_is_hidden_from_the_pane_when_turned_off() {
        let spec = spec_of("negative-volume-index");
        let bars = synthetic_bars(50, true);
        let mut inputs = spec.default_inputs();
        inputs.insert("showSignal".into(), false.into());
        let out = spec.compute(&bars, &inputs);
        assert_eq!(out.plots[0].key, "signal");
        assert_eq!(out.plots[0].display.and_then(|d| d.pane), Some(false));
        assert_eq!(out.plots[1].key, "nvi");
        assert_eq!(out.bands.len(), 1);
    }

    #[test]
    fn money_flow_index_is_bounded() {
        let bars = synthetic_bars(120, true);
        let mfi = plot_values(&spec_of("money-flow-index"), &bars, "mfi");
        let finite: Vec<f64> = mfi.iter().copied().filter(|v| v.is_finite()).collect();
        assert!(!finite.is_empty());
        assert!(finite.iter().all(|v| (-1e-9..=100.0 + 1e-9).contains(v)));
    }

    #[test]
    fn pvo_plots_histogram_and_signal_before_the_baseline_anchor() {
        let spec = spec_of("pvo");
        let out = spec.compute(&synthetic_bars(80, true), &spec.default_inputs());
        let keys: Vec<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, vec!["pvo", "hist", "signal", "baseline"]);
        assert_eq!(out.plots[1].kind, LineLikeKind::Columns);
        assert_eq!(out.plots[1].base, Some(0.0));
        assert_eq!(spec.inputs[0].group.as_deref(), Some(OSCILLATOR));
    }

    #[test]
    fn intraday_intensity_cumulative_mode_is_a_single_line() {
        let spec = spec_of("intraday-intensity");
        let mut inputs = spec.default_inputs();
        inputs.insert("mode".into(), II_CUMULATIVE.into());
        let out = spec.compute(&synthetic_bars(50, true), &inputs);
        assert_eq!(out.plots.len(), 1);
        assert!(out.levels.is_empty() && out.bands.is_empty());
    }

    #[test]
    fn volume_flow_indicator_length_inputs_start_at_two() {
        let spec = spec_of("volume-flow-indicator");
        let mins: Vec<Option<f64>> = spec
            .inputs
            .iter()
            .filter(|i| i.key == "length" || i.key == "volLength")
            .map(|i| i.min)
            .collect();
        assert_eq!(mins, vec![Some(2.0), Some(2.0)]);
    }
}
