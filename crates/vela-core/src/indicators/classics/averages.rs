//! Moving-average family: overlays that smooth a price source.

use crate::i18n::{MessageLike, msg};
use crate::model::{LineStyle, Ohlcv};
use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL, WARNING};

use super::define::{
    ClassicIndicatorSpec, ClassicInput, ClassicLevel, ClassicOutput, ClassicPlot, flag, len, num,
    text,
};
use super::math::{
    HAMMING_A0, HAMMING_A1, KAMA_FAST_ALPHA, KAMA_SLOW_ALPHA, alma, atr, cmo, dema, ema, hamming,
    hma, js_min, js_round, kama, linreg, map, mcginley, rma, shift, sma, source_values, stdev,
    tema, volumes, vwma, wma, zip,
};
use super::shared::{
    DirectionInk, MAX_LENGTH, Parts, SETTINGS, STYLE, bool_input, channel_style_inputs,
    color_input, color_input_default, directional_channel, float_input, gradient, int_input,
    length_input, option_input, source_input, transp, width_input_default,
};

/// Every smoothing the catalog's generic average offers, in the reference's order.
pub const MA_TYPES: [&str; 13] = [
    "SMA",
    "EMA",
    "WMA",
    "RMA (SMMA)",
    "HMA",
    "ALMA",
    "VWMA",
    "DEMA",
    "TEMA",
    "KAMA",
    "LSMA",
    "McGinley",
    "Hamming",
];

/// The subset an envelope basis offers.
const ENVELOPE_MA_TYPES: [&str; 6] = ["SMA", "EMA", "WMA", "RMA (SMMA)", "HMA", "VWMA"];

/// Tuning of the types that take extra parameters; `None` keeps the type's own default.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaOptions {
    pub alma_offset: Option<f64>,
    pub alma_sigma: Option<f64>,
    pub lsma_offset: Option<f64>,
}

/// Resolve one of [`MA_TYPES`] to its values. `"RMA"` is accepted for stored
/// settings written before the rename; an unknown name falls back to the SMA.
pub fn moving_average_of(
    kind: &str,
    src: &[f64],
    bars: &[Ohlcv],
    length: usize,
    opts: MaOptions,
) -> Vec<f64> {
    match kind {
        "EMA" => ema(src, length),
        "WMA" => wma(src, length),
        "RMA" | "RMA (SMMA)" => rma(src, length),
        "HMA" => hma(src, length),
        "ALMA" => alma(
            src,
            length,
            opts.alma_offset.unwrap_or(0.85),
            opts.alma_sigma.unwrap_or(6.0),
        ),
        "VWMA" => vwma(src, &volumes(bars), length),
        "DEMA" => dema(src, length),
        "TEMA" => tema(src, length),
        "KAMA" => kama(src, length, KAMA_FAST_ALPHA, KAMA_SLOW_ALPHA),
        "LSMA" => linreg(src, length, opts.lsma_offset.unwrap_or(0.0)),
        "McGinley" => mcginley(src, length),
        "Hamming" => hamming(src, length, HAMMING_A0, HAMMING_A1),
        _ => sma(src, length),
    }
}

/// A bar displacement from a numeric input: truncated toward zero. Anything
/// beyond a billion bars displaces every value out of range anyway, so the clamp
/// only keeps the arithmetic in `shift` from overflowing.
fn bar_offset(inputs: &crate::model::Inputs, key: &str) -> i64 {
    num(inputs, key, 0.0).trunc().clamp(-1e9, 1e9) as i64
}

/// Per-bar ink from the line's own slope: rising bullish, falling bearish, flat neutral.
fn slope_colors(values: &[f64], bull: &str, bear: &str, flat: &str) -> Vec<Option<String>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            if !x.is_finite() {
                return None;
            }
            let prev = if i > 0 { values[i - 1] } else { f64::NAN };
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

/// The bullish/bearish/neutral trio every slope-colored average shares.
fn direction_inputs() -> Vec<ClassicInput> {
    vec![
        bool_input(
            "trendColor",
            msg("indicator.slope.trendColor", "Slope Trend Coloring"),
            true,
            STYLE,
            Some(msg(
                "indicator.slope.trendColor.tooltip",
                "Colors the line by its slope — bullish while it rises, bearish while it falls. Disable to draw it in the neutral color.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.common.bullColor", "Bullish"),
            "bullColor",
            None,
        ),
        color_input(
            BEARISH,
            msg("indicator.common.bearColor", "Bearish"),
            "bearColor",
            None,
        ),
        color_input(
            NEUTRAL,
            msg("indicator.common.neutralColor", "Neutral"),
            "neutralColor",
            None,
        ),
        width_input_default(2.0),
    ]
}

fn slope_colored_plot(
    key: &str,
    title: MessageLike,
    values: Vec<f64>,
    inputs: &crate::model::Inputs,
) -> ClassicPlot {
    let neutral = text(inputs, "neutralColor", NEUTRAL);
    let colored = flag(inputs, "trendColor", true);
    let colors = colored.then(|| {
        slope_colors(
            &values,
            &text(inputs, "bullColor", BULLISH),
            &text(inputs, "bearColor", BEARISH),
            &neutral,
        )
    });
    let plot = ClassicPlot::new(key, title, values, neutral).width(num(inputs, "lineWidth", 2.0));
    match colors {
        Some(colors) => plot.colors(colors),
        None => plot,
    }
}

fn offset_input() -> ClassicInput {
    int_input(
        "offset",
        msg("indicator.common.offset", "Offset"),
        0.0,
        -500.0,
        500.0,
        Some(msg(
            "indicator.common.offset.tooltip",
            "Bars the plotted line is displaced by. Positive values push it to the right.",
        )),
    )
}

/// The text of a single-kind average: its names and the tooltips of its length and source.
struct FixedAverageText {
    title: MessageLike,
    short_title: MessageLike,
    length_tooltip: MessageLike,
    source_tooltip: MessageLike,
}

/// A single-kind average: the same inputs as the generic study minus the type picker.
fn fixed_average(
    indicator_type: &'static str,
    text_of: FixedAverageText,
    plot_key: &'static str,
    smooth: fn(&[f64], usize) -> Vec<f64>,
) -> ClassicIndicatorSpec {
    let short_title = text_of.short_title.clone();
    let plot_title = text_of.short_title;
    ClassicIndicatorSpec::new(
        indicator_type,
        text_of.title,
        true,
        vec![
            length_input(
                20.0,
                "length",
                msg("indicator.common.length", "Length"),
                MAX_LENGTH,
                Some(text_of.length_tooltip),
            ),
            source_input(
                "Close",
                "source",
                msg("indicator.common.source", "Source"),
                Some(text_of.source_tooltip),
            ),
            offset_input(),
            color_input_default(),
        ],
        move |bars, inputs| {
            let mut values = smooth(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "length", 20),
            );
            let offset = bar_offset(inputs, "offset");
            if offset != 0 {
                values = shift(&values, offset);
            }
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new(
                        plot_key,
                        plot_title.clone(),
                        values,
                        text(inputs, "color", INFO),
                    )
                    .width(2.0),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(short_title)
}

fn simple_ma() -> ClassicIndicatorSpec {
    fixed_average(
        "sma",
        FixedAverageText {
            title: msg("indicator.sma.title", "Simple Moving Average"),
            short_title: msg("indicator.sma.shortTitle", "SMA"),
            length_tooltip: msg(
                "indicator.sma.length.tooltip",
                "Number of bars used in the SMA calculation.",
            ),
            source_tooltip: msg(
                "indicator.sma.source.tooltip",
                "Price series the SMA is calculated on.",
            ),
        },
        "sma",
        sma,
    )
}

fn exponential_ma() -> ClassicIndicatorSpec {
    fixed_average(
        "ema",
        FixedAverageText {
            title: msg("indicator.ema.title", "Exponential Moving Average"),
            short_title: msg("indicator.ema.shortTitle", "EMA"),
            length_tooltip: msg(
                "indicator.ema.length.tooltip",
                "Number of bars used in the EMA calculation.",
            ),
            source_tooltip: msg(
                "indicator.ema.source.tooltip",
                "Price series the EMA is calculated on.",
            ),
        },
        "ema",
        ema,
    )
}

fn moving_average() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "moving-average",
        msg("indicator.movingAverage.title", "Moving Average"),
        true,
        vec![
            option_input("maType", msg("indicator.movingAverage.maType", "MA 1 Type"), "SMA", &MA_TYPES, Some(msg("indicator.movingAverage.maType.tooltip", "Moving average calculation used for MA 1."))),
            length_input(20.0, "length", msg("indicator.movingAverage.length", "MA 1 Length"), MAX_LENGTH, Some(msg("indicator.movingAverage.length.tooltip", "Number of bars used in the MA 1 calculation."))),
            source_input("Close", "source", msg("indicator.movingAverage.source", "MA 1 Source"), Some(msg("indicator.movingAverage.source.tooltip", "Price series MA 1 is calculated on."))),
            bool_input("showMa2", msg("indicator.movingAverage.showMa2", "Show MA 2"), false, SETTINGS, Some(msg("indicator.movingAverage.showMa2.tooltip", "Displays the optional second moving average."))),
            option_input("ma2Type", msg("indicator.movingAverage.ma2Type", "MA 2 Type"), "SMA", &MA_TYPES, Some(msg("indicator.movingAverage.ma2Type.tooltip", "Moving average calculation used for MA 2."))),
            length_input(50.0, "ma2Length", msg("indicator.movingAverage.ma2Length", "MA 2 Length"), MAX_LENGTH, Some(msg("indicator.movingAverage.ma2Length.tooltip", "Number of bars used in the MA 2 calculation."))),
            source_input("Close", "ma2Source", msg("indicator.movingAverage.ma2Source", "MA 2 Source"), Some(msg("indicator.movingAverage.ma2Source.tooltip", "Price series MA 2 is calculated on."))),
            float_input(
                "almaOffset",
                msg("indicator.movingAverage.almaOffset", "ALMA Offset"),
                0.85,
                0.0,
                1.0,
                0.05,
                Some(msg("indicator.movingAverage.almaOffset.tooltip", "ALMA type only. Values near 1 make the average more responsive, values near 0 make it smoother.")),
            ),
            float_input(
                "almaSigma",
                msg("indicator.movingAverage.almaSigma", "ALMA Sigma"),
                6.0,
                0.1,
                100.0,
                0.5,
                Some(msg("indicator.movingAverage.almaSigma.tooltip", "ALMA type only. Larger values widen the Gaussian window for a smoother average.")),
            ),
            int_input(
                "lsmaOffset",
                msg("indicator.movingAverage.lsmaOffset", "LSMA Offset"),
                0.0,
                0.0,
                500.0,
                Some(msg("indicator.movingAverage.lsmaOffset.tooltip", "LSMA type only. Bar offset applied to the linear regression value.")),
            ),
            offset_input(),
            color_input(INFO, msg("indicator.movingAverage.color", "MA 1"), "color", Some(msg("indicator.movingAverage.color.tooltip", "MA 1 line color."))),
            color_input(WARNING, msg("indicator.movingAverage.ma2Color", "MA 2"), "ma2Color", Some(msg("indicator.movingAverage.ma2Color.tooltip", "MA 2 line color."))),
        ],
        |bars, inputs| {
            let opts = MaOptions {
                alma_offset: Some(num(inputs, "almaOffset", 0.85)),
                alma_sigma: Some(num(inputs, "almaSigma", 6.0)),
                lsma_offset: Some(num(inputs, "lsmaOffset", 0.0).trunc()),
            };
            let offset = bar_offset(inputs, "offset");
            let displace = |v: Vec<f64>| if offset == 0 { v } else { shift(&v, offset) };
            let kind = text(inputs, "maType", "SMA");
            let ma1 = displace(moving_average_of(&kind, &source_values(bars, &text(inputs, "source", "Close")), bars, len(inputs, "length", 20), opts));
            let mut plots = vec![ClassicPlot::new("ma", kind, ma1, text(inputs, "color", INFO)).width(2.0)];
            if flag(inputs, "showMa2", false) {
                let kind2 = text(inputs, "ma2Type", "SMA");
                let ma2 = displace(moving_average_of(&kind2, &source_values(bars, &text(inputs, "ma2Source", "Close")), bars, len(inputs, "ma2Length", 50), opts));
                plots.push(ClassicPlot::new("ma2", format!("{kind2} 2"), ma2, text(inputs, "ma2Color", WARNING)).width(2.0));
            }
            ClassicOutput { plots, ..Default::default() }
        },
    )
    .short_title(msg("indicator.movingAverage.shortTitle", "MA"))
}

fn smoothed_ma() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        source_input(
            "Close",
            "source",
            msg("indicator.rma.source", "Source"),
            Some(msg(
                "indicator.rma.source.tooltip",
                "Price series the average smooths. Wilder computed his 1978 indicators on the close, but the recursion accepts any source.",
            )),
        ),
        length_input(
            14.0,
            "length",
            msg("indicator.rma.length", "RMA Length"),
            MAX_LENGTH,
            Some(msg(
                "indicator.rma.length.tooltip",
                "Number of bars N in Wilder's smoothing: the line keeps (N − 1)/N of its previous value and blends in 1/N of the new price. That alpha is smaller than an EMA's, so an N-period RMA responds like a (2N − 1)-period EMA.",
            )),
        ),
    ];
    inputs.extend(direction_inputs());
    ClassicIndicatorSpec::new(
        "rma",
        msg("indicator.rma.title", "Smoothed Moving Average"),
        true,
        inputs,
        |bars, inputs| {
            let values = rma(
                &source_values(bars, &text(inputs, "source", "Close")),
                len(inputs, "length", 14),
            );
            ClassicOutput {
                plots: vec![slope_colored_plot(
                    "rma",
                    msg("indicator.rma.plot.rma", "RMA"),
                    values,
                    inputs,
                )],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.rma.shortTitle", "RMA"))
}

fn zlema() -> ClassicIndicatorSpec {
    let mut inputs = vec![
        source_input(
            "Close",
            "source",
            msg("indicator.zlema.source", "Source"),
            Some(msg(
                "indicator.zlema.source.tooltip",
                "Price series the average de-lags and then smooths.",
            )),
        ),
        length_input(
            21.0,
            "length",
            msg("indicator.zlema.length", "ZLEMA Length"),
            MAX_LENGTH,
            Some(msg(
                "indicator.zlema.length.tooltip",
                "EMA length N. It sets both the smoothing (alpha = 2 / (N + 1)) and the correction: an EMA of length N lags by about (N − 1)/2 bars, and the de-lagging step is sized to cancel exactly that.",
            )),
        ),
    ];
    inputs.extend(direction_inputs());
    ClassicIndicatorSpec::new(
        "zlema",
        msg(
            "indicator.zlema.title",
            "Zero-Lag Exponential Moving Average",
        ),
        true,
        inputs,
        |bars, inputs| {
            let length = len(inputs, "length", 21);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let lag = js_round((length as f64 - 1.0) / 2.0) as i64;
            let delagged = zip(&src, &shift(&src, lag), |a, b| 2.0 * a - b);
            ClassicOutput {
                plots: vec![slope_colored_plot(
                    "zlema",
                    msg("indicator.zlema.plot.zlema", "ZLEMA"),
                    ema(&delagged, length),
                    inputs,
                )],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.zlema.shortTitle", "ZLEMA"))
}

const MOMENTUM_SCALER: &str = "Momentum (|CMO|)";
const VOLATILITY_SCALER: &str = "Volatility (StDev Ratio)";
const REGIME: &str = "Trend Regime";

fn vidya() -> ClassicIndicatorSpec {
    ClassicIndicatorSpec::new(
        "vidya",
        msg("indicator.vidya.title", "Variable Index Dynamic Average"),
        true,
        vec![
            source_input("Close", "source", msg("indicator.vidya.source", "Source"), Some(msg("indicator.vidya.source.tooltip", "Price series the average smooths. VIDYA is classically computed on the close."))),
            length_input(
                12.0,
                "length",
                msg("indicator.vidya.length", "VIDYA Length"),
                MAX_LENGTH,
                Some(msg("indicator.vidya.length.tooltip", "EMA-equivalent length N of the base smoothing constant, alpha = 2 / (N + 1). The adaptive scaler rescales this base speed bar by bar.")),
            ),
            option_input(
                "scaler",
                msg("indicator.vidya.scaler", "Adaptive Scaler"),
                MOMENTUM_SCALER,
                &[MOMENTUM_SCALER, VOLATILITY_SCALER],
                Some(msg(
                    "indicator.vidya.scaler.tooltip",
                    "Bar-by-bar measure that rescales the smoothing constant. Momentum divides the absolute Chande Momentum Oscillator by 100; Volatility is Chande's original short-term over reference standard deviation.",
                )),
            ),
            int_input(
                "scalerLength",
                msg("indicator.vidya.scalerLength", "Scaler Lookback"),
                9.0,
                1.0,
                MAX_LENGTH,
                Some(msg("indicator.vidya.scalerLength.tooltip", "Lookback of the adaptive scaler — the CMO period in momentum mode, or the short-term standard deviation window in volatility mode.")),
            ),
            int_input(
                "refLength",
                msg("indicator.vidya.refLength", "Reference StDev Length"),
                30.0,
                2.0,
                MAX_LENGTH,
                Some(msg("indicator.vidya.refLength.tooltip", "Volatility mode only — window of the reference standard deviation the short-term reading is divided by.")),
            ),
            float_input(
                "flatThreshold",
                msg("indicator.vidya.flatThreshold", "Flat Threshold"),
                0.05,
                0.0,
                10.0,
                0.01,
                Some(msg("indicator.vidya.flatThreshold.tooltip", "One-bar change of VIDYA, as a fraction of ATR, below which the line reads as flat.")),
            )
            .group(REGIME),
            int_input(
                "atrLength",
                msg("indicator.vidya.atrLength", "ATR Normalization Length"),
                14.0,
                1.0,
                MAX_LENGTH,
                Some(msg("indicator.vidya.atrLength.tooltip", "ATR window used to normalize VIDYA's one-bar slope, so the flat threshold behaves consistently across symbols and timeframes.")),
            )
            .group(REGIME),
            bool_input(
                "trendColor",
                msg("indicator.vidya.trendColor", "Trend Regime Coloring"),
                true,
                STYLE,
                Some(msg("indicator.vidya.trendColor.tooltip", "Colors VIDYA by its regime read — bullish while sloping up beyond the flat threshold, bearish while sloping down, flat inside congestion.")),
            ),
            bool_input(
                "adaptiveGradient",
                msg("indicator.vidya.adaptiveGradient", "Adaptive Gradient"),
                true,
                STYLE,
                Some(msg("indicator.vidya.adaptiveGradient.tooltip", "Blends the trend color toward the flat color according to the adaptive scaler, so the line pales as momentum dies.")),
            ),
            color_input(BULLISH, msg("indicator.vidya.bullColor", "Bullish"), "bullColor", None),
            color_input(BEARISH, msg("indicator.vidya.bearColor", "Bearish"), "bearColor", None),
            color_input(NEUTRAL, msg("indicator.vidya.flatColor", "Flat"), "flatColor", None),
            color_input(NEUTRAL, msg("indicator.vidya.neutralColor", "Neutral"), "neutralColor", None),
            width_input_default(2.0),
        ],
        |bars, inputs| {
            let alpha_length = num(inputs, "length", 12.0);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let scaler_len = len(inputs, "scalerLength", 9);
            let momentum = map(&cmo(&src, scaler_len), |x| x.abs() / 100.0);
            let short_dev = stdev(&src, scaler_len);
            let ref_dev = stdev(&src, len(inputs, "refLength", 30));
            let volatility = zip(&short_dev, &ref_dev, |s, r| if r > 0.0 { s / r } else { 0.0 });
            let scale = if text(inputs, "scaler", MOMENTUM_SCALER) == VOLATILITY_SCALER { volatility } else { momentum };
            let alpha_base = 2.0 / (alpha_length + 1.0);
            let mut out = vec![f64::NAN; src.len()];
            let mut prev = f64::NAN;
            for (i, &x) in src.iter().enumerate() {
                if !x.is_finite() {
                    continue;
                }
                let k = scale[i];
                if !prev.is_finite() || !k.is_finite() {
                    prev = x;
                } else {
                    let alpha = js_min(alpha_base * k, 1.0);
                    prev = alpha * x + (1.0 - alpha) * prev;
                }
                out[i] = prev;
            }
            let neutral = text(inputs, "neutralColor", NEUTRAL);
            let width = num(inputs, "lineWidth", 2.0);
            let title = msg("indicator.vidya.plot.vidya", "VIDYA");
            if !flag(inputs, "trendColor", true) {
                return ClassicOutput { plots: vec![ClassicPlot::new("vidya", title, out, neutral).width(width)], ..Default::default() };
            }
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let flat = text(inputs, "flatColor", NEUTRAL);
            let threshold = num(inputs, "flatThreshold", 0.05);
            let range = atr(bars, len(inputs, "atrLength", 14));
            let fade = flag(inputs, "adaptiveGradient", true);
            let colors: Vec<Option<String>> = out
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    if !x.is_finite() {
                        return None;
                    }
                    let r = range[i];
                    let slope = if i > 0 && out[i - 1].is_finite() { x - out[i - 1] } else { 0.0 };
                    let normalized = if r.is_finite() && r != 0.0 { slope / r } else { 0.0 };
                    let ink = if normalized > threshold {
                        &bull
                    } else if normalized < -threshold {
                        &bear
                    } else {
                        return Some(flat.clone());
                    };
                    Some(if fade { gradient(js_min(scale[i], 1.0), 0.0, 1.0, &flat, ink) } else { ink.clone() })
                })
                .collect();
            ClassicOutput { plots: vec![ClassicPlot::new("vidya", title, out, flat).width(width).colors(colors)], ..Default::default() }
        },
    )
    .short_title(msg("indicator.vidya.shortTitle", "VIDYA"))
}

fn parts_output(parts: Parts) -> ClassicOutput {
    ClassicOutput {
        plots: parts.plots,
        bands: parts.bands,
        levels: parts.levels,
        ..Default::default()
    }
}

fn ma_envelope() -> ClassicIndicatorSpec {
    let mut length = length_input(
        20.0,
        "length",
        msg("indicator.maEnvelope.length", "Length"),
        MAX_LENGTH,
        Some(msg(
            "indicator.maEnvelope.length.tooltip",
            "Number of bars in the basis moving average.",
        )),
    );
    length.min = Some(2.0);
    let mut inputs = vec![
        option_input(
            "maType",
            msg("indicator.maEnvelope.maType", "MA Type"),
            "SMA",
            &ENVELOPE_MA_TYPES,
            Some(msg(
                "indicator.maEnvelope.maType.tooltip",
                "Moving average type used as the envelope basis.",
            )),
        ),
        length,
        source_input(
            "Close",
            "source",
            msg("indicator.maEnvelope.source", "Source"),
            Some(msg(
                "indicator.maEnvelope.source.tooltip",
                "Price series the basis moving average is calculated on.",
            )),
        ),
        float_input(
            "percent",
            msg("indicator.maEnvelope.percent", "Percent"),
            2.5,
            0.0,
            50.0,
            0.1,
            Some(msg(
                "indicator.maEnvelope.percent.tooltip",
                "Envelope offset as a percentage of the basis. Upper = basis × (1 + percent / 100), lower = basis × (1 − percent / 100).",
            )),
        ),
    ];
    inputs.extend(channel_style_inputs(None));
    ClassicIndicatorSpec::new(
        "ma-envelope",
        msg("indicator.maEnvelope.title", "Moving Average Envelope"),
        true,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let basis = moving_average_of(
                &text(inputs, "maType", "SMA"),
                &src,
                bars,
                len(inputs, "length", 20),
                MaOptions::default(),
            );
            let k = num(inputs, "percent", 2.5) / 100.0;
            let ink = DirectionInk {
                bull: text(inputs, "bullColor", BULLISH),
                bear: text(inputs, "bearColor", BEARISH),
                neutral: text(inputs, "basisColor", NEUTRAL),
            };
            parts_output(directional_channel(
                bars,
                &basis,
                &map(&basis, |x| x * (1.0 + k)),
                &map(&basis, |x| x * (1.0 - k)),
                &ink,
                "envelope",
            ))
        },
    )
    .short_title(msg("indicator.maEnvelope.shortTitle", "MA Env"))
}

fn least_squares_ma() -> ClassicIndicatorSpec {
    let mut length = length_input(
        25.0,
        "length",
        msg("indicator.lsma.length", "LSMA Length"),
        MAX_LENGTH,
        Some(msg(
            "indicator.lsma.length.tooltip",
            "Number of bars N in each least-squares fit. Every bar fits a straight line to its last N values and plots where that line ends, so the average leads turns that a same-length SMA trails, at the cost of overshooting after sharp moves.",
        )),
    );
    length.min = Some(2.0);
    let mut inputs = vec![
        source_input(
            "Close",
            "source",
            msg("indicator.lsma.source", "Source"),
            Some(msg(
                "indicator.lsma.source.tooltip",
                "Price series the regression line is fitted to.",
            )),
        ),
        length,
        int_input(
            "offset",
            msg("indicator.lsma.offset", "Regression Offset"),
            0.0,
            0.0,
            500.0,
            Some(msg(
                "indicator.lsma.offset.tooltip",
                "Bars to step back along each bar's fitted line. 0 plots the line's endpoint at the current bar; larger values read the fit further back, trading lead for smoothness.",
            )),
        ),
    ];
    inputs.extend(direction_inputs());
    ClassicIndicatorSpec::new(
        "lsma",
        msg("indicator.lsma.title", "Least Squares Moving Average"),
        true,
        inputs,
        |bars, inputs| {
            let src = source_values(bars, &text(inputs, "source", "Close"));
            let values = linreg(
                &src,
                len(inputs, "length", 25),
                num(inputs, "offset", 0.0).trunc(),
            );
            ClassicOutput {
                plots: vec![slope_colored_plot(
                    "lsma",
                    msg("indicator.lsma.plot.lsma", "LSMA"),
                    values,
                    inputs,
                )],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.lsma.shortTitle", "LSMA"))
}

fn linear_regression_slope() -> ClassicIndicatorSpec {
    let mut length = length_input(
        100.0,
        "length",
        msg("indicator.linearRegressionSlope.length", "Length"),
        MAX_LENGTH,
        Some(msg(
            "indicator.linearRegressionSlope.length.tooltip",
            "Number of bars used to fit the linear regression.",
        )),
    );
    length.min = Some(2.0);
    ClassicIndicatorSpec::new(
        "linear-regression-slope",
        msg(
            "indicator.linearRegressionSlope.title",
            "Linear Regression Slope",
        ),
        false,
        vec![
            length,
            source_input(
                "Close",
                "source",
                msg("indicator.linearRegressionSlope.source", "Source"),
                Some(msg(
                    "indicator.linearRegressionSlope.source.tooltip",
                    "Price series the regression is fitted to.",
                )),
            ),
            color_input(
                BULLISH,
                msg("indicator.linearRegressionSlope.bullColor", "Rising"),
                "bullColor",
                Some(msg(
                    "indicator.linearRegressionSlope.bullColor.tooltip",
                    "Color of the slope line while the fit slopes upward.",
                )),
            ),
            color_input(
                BEARISH,
                msg("indicator.linearRegressionSlope.bearColor", "Falling"),
                "bearColor",
                Some(msg(
                    "indicator.linearRegressionSlope.bearColor.tooltip",
                    "Color of the slope line while the fit slopes downward.",
                )),
            ),
            color_input(
                NEUTRAL,
                msg("indicator.linearRegressionSlope.lineColor", "Slope Line"),
                "lineColor",
                Some(msg(
                    "indicator.linearRegressionSlope.lineColor.tooltip",
                    "Color of the slope line while the fit is flat.",
                )),
            ),
        ],
        |bars, inputs| {
            let length = len(inputs, "length", 100);
            let src = source_values(bars, &text(inputs, "source", "Close"));
            // Price change per bar along the current fit: its endpoint minus the point one bar back.
            let slope = zip(
                &linreg(&src, length, 0.0),
                &linreg(&src, length, 1.0),
                |a, b| a - b,
            );
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let flat = text(inputs, "lineColor", NEUTRAL);
            let colors: Vec<Option<String>> = slope
                .iter()
                .map(|&s| {
                    s.is_finite().then(|| {
                        if s > 0.0 {
                            bull.clone()
                        } else if s < 0.0 {
                            bear.clone()
                        } else {
                            flat.clone()
                        }
                    })
                })
                .collect();
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new(
                        "slope",
                        msg("indicator.linearRegressionSlope.plot.slope", "Slope"),
                        slope,
                        flat,
                    )
                    .width(2.0)
                    .colors(colors),
                ],
                levels: vec![
                    ClassicLevel::new("zero", 0.0)
                        .color(transp(NEUTRAL, 50.0))
                        .line_style(LineStyle::Dashed),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.linearRegressionSlope.shortTitle", "LRS"))
}

/// Every moving-average study, in the original's order.
pub fn average_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        simple_ma(),
        exponential_ma(),
        moving_average(),
        smoothed_ma(),
        zlema(),
        vidya(),
        ma_envelope(),
        least_squares_ma(),
        linear_regression_slope(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::classics::math::closes;
    use crate::model::InputValue;

    fn bars(n: usize) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| {
                let c = 100.0 + 10.0 * (i as f64 / 15.0).sin() + (i % 7) as f64;
                Ohlcv::new(
                    i as i64 * 60_000,
                    c,
                    c + 1.0,
                    c - 1.0,
                    c,
                    Some(1000.0 + (i % 50) as f64 * 10.0),
                )
            })
            .collect()
    }

    fn spec(kind: &str) -> ClassicIndicatorSpec {
        average_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .expect("spec exists")
    }

    fn assert_same(a: &[f64], b: &[f64]) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!(x == y || (x.is_nan() && y.is_nan()), "{x} != {y}");
        }
    }

    #[test]
    fn ships_the_catalog_in_order() {
        let types: Vec<&str> = average_specs().iter().map(|s| s.indicator_type).collect();
        assert_eq!(
            types,
            [
                "sma",
                "ema",
                "moving-average",
                "rma",
                "zlema",
                "vidya",
                "ma-envelope",
                "lsma",
                "linear-regression-slope"
            ]
        );
    }

    #[test]
    fn every_spec_computes_bar_aligned_plots_on_defaults() {
        let data = bars(400);
        for s in average_specs() {
            let out = s.compute(&data, &s.default_inputs());
            assert!(!out.plots.is_empty(), "{}", s.indicator_type);
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    data.len(),
                    "{}:{}",
                    s.indicator_type,
                    plot.key
                );
                if let Some(colors) = &plot.colors {
                    assert_eq!(
                        colors.len(),
                        data.len(),
                        "{}:{}",
                        s.indicator_type,
                        plot.key
                    );
                }
            }
        }
    }

    #[test]
    fn every_ma_type_computes_aligned_values() {
        let data = bars(300);
        let s = spec("moving-average");
        for kind in MA_TYPES {
            let mut inputs = s.default_inputs();
            inputs.insert("maType".into(), InputValue::from(kind));
            inputs.insert("showMa2".into(), InputValue::from(true));
            inputs.insert("ma2Type".into(), InputValue::from(kind));
            let out = s.compute(&data, &inputs);
            assert_eq!(out.plots.len(), 2, "{kind}");
            assert_eq!(out.plots[1].key, "ma2");
            assert!(out.plots.iter().all(|p| p.values.len() == data.len() && p.values.iter().any(|v| v.is_finite())), "{kind}");
        }
    }

    #[test]
    fn simple_and_exponential_studies_match_the_math() {
        let data = bars(60);
        let closes = closes(&data);
        let sma_spec = spec("sma");
        let ema_spec = spec("ema");
        let keys =
            |s: &ClassicIndicatorSpec| s.inputs.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&sma_spec), ["length", "source", "offset", "color"]);
        assert_eq!(keys(&ema_spec), ["length", "source", "offset", "color"]);
        let sma_out = sma_spec.compute(&data, &sma_spec.default_inputs());
        let ema_out = ema_spec.compute(&data, &ema_spec.default_inputs());
        assert_same(&sma_out.plots[0].values, &sma(&closes, 20));
        assert_same(&ema_out.plots[0].values, &ema(&closes, 20));
        assert_eq!(sma_out.plots[0].title.source_text(), "SMA");
        assert_eq!(ema_out.plots[0].title.source_text(), "EMA");

        let mut inputs = sma_spec.default_inputs();
        inputs.insert("offset".into(), InputValue::from(2.0));
        let shifted = sma_spec.compute(&data, &inputs);
        assert_eq!(shifted.plots[0].values[30], sma_out.plots[0].values[28]);
        assert!(shifted.plots[0].values[1].is_nan());
    }

    #[test]
    fn lsma_follows_the_regression_and_its_offset() {
        let data = bars(200);
        let closes = closes(&data);
        let s = spec("lsma");
        assert!(s.overlay);
        let out = s.compute(&data, &s.default_inputs());
        assert_eq!(out.plots.len(), 1);
        assert_same(&out.plots[0].values, &linreg(&closes, 25, 0.0));
        let mut inputs = s.default_inputs();
        inputs.insert("length".into(), InputValue::from(30.0));
        inputs.insert("offset".into(), InputValue::from(3.0));
        assert_same(
            &s.compute(&data, &inputs).plots[0].values,
            &linreg(&closes, 30, 3.0),
        );
    }

    #[test]
    fn regression_slope_is_the_fit_endpoint_minus_one_bar_back() {
        let data = bars(200);
        let closes = closes(&data);
        let s = spec("linear-regression-slope");
        assert!(!s.overlay);
        let out = s.compute(&data, &s.default_inputs());
        let curve = linreg(&closes, 100, 0.0);
        let prior = linreg(&closes, 100, 1.0);
        assert!((out.plots[0].values[150] - (curve[150] - prior[150])).abs() < 1e-10);
        assert!(out.plots[0].values[98].is_nan());
        assert_eq!(
            out.levels.iter().map(|l| l.price).collect::<Vec<_>>(),
            [0.0]
        );
    }

    #[test]
    fn rma_study_equals_the_math_and_colors_by_slope() {
        let data = bars(120);
        let s = spec("rma");
        let out = s.compute(&data, &s.default_inputs());
        assert_same(&out.plots[0].values, &rma(&closes(&data), 14));
        let colors = out.plots[0].colors.as_ref().expect("slope colors");
        assert_eq!(colors[0], None);
        assert_eq!(colors[13].as_deref(), Some(NEUTRAL));
        let mut inputs = s.default_inputs();
        inputs.insert("trendColor".into(), InputValue::from(false));
        assert!(s.compute(&data, &inputs).plots[0].colors.is_none());
    }

    #[test]
    fn zlema_delags_before_smoothing() {
        let data = bars(120);
        let c = closes(&data);
        let s = spec("zlema");
        let out = s.compute(&data, &s.default_inputs());
        let delagged = zip(&c, &shift(&c, 10), |a, b| 2.0 * a - b);
        assert_same(&out.plots[0].values, &ema(&delagged, 21));
    }

    #[test]
    fn vidya_seeds_on_the_first_value_and_stays_bounded() {
        let data = bars(200);
        let s = spec("vidya");
        let out = s.compute(&data, &s.default_inputs());
        let values = &out.plots[0].values;
        assert_eq!(values[0], data[0].close);
        let (lo, hi) = data
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), b| {
                (lo.min(b.close), hi.max(b.close))
            });
        assert!(
            values
                .iter()
                .all(|v| v.is_finite() && *v >= lo - 1e-9 && *v <= hi + 1e-9)
        );
        let mut inputs = s.default_inputs();
        inputs.insert("scaler".into(), InputValue::from(VOLATILITY_SCALER));
        assert_eq!(s.compute(&data, &inputs).plots[0].values.len(), data.len());
    }

    #[test]
    fn envelope_brackets_the_basis_by_the_percent() {
        let data = bars(100);
        let s = spec("ma-envelope");
        let out = s.compute(&data, &s.default_inputs());
        let keys: Vec<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["basis", "upper", "lower"]);
        let (basis, upper, lower) = (
            &out.plots[0].values,
            &out.plots[1].values,
            &out.plots[2].values,
        );
        assert_same(basis, &sma(&closes(&data), 20));
        assert!((upper[50] - basis[50] * 1.025).abs() < 1e-9);
        assert!((lower[50] - basis[50] * 0.975).abs() < 1e-9);
        assert_eq!(out.bands.len(), 1);
    }

    #[test]
    fn moving_average_of_falls_back_to_sma_and_accepts_the_legacy_rma_name() {
        let data = bars(50);
        let c = closes(&data);
        assert_same(
            &moving_average_of("nonsense", &c, &data, 5, MaOptions::default()),
            &sma(&c, 5),
        );
        assert_same(
            &moving_average_of("RMA", &c, &data, 5, MaOptions::default()),
            &rma(&c, 5),
        );
        assert_same(
            &moving_average_of("RMA (SMMA)", &c, &data, 5, MaOptions::default()),
            &rma(&c, 5),
        );
    }
}
