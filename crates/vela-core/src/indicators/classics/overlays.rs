//! Price-anchored specials: stops, anchored averages, levels, pivots, patterns.

use std::collections::{HashMap, VecDeque};

use crate::i18n::{msg, with_values};
use crate::model::{
    DrawingXLoc, InputType, LineLikeKind, LineStyle, MarkerPoint, MarkerPosition, Ohlcv,
    PolylinePoint,
};
use crate::palette::{BEARISH, BULLISH, NEUTRAL, WARNING};

use super::define::{
    ClassicBand, ClassicIndicatorSpec, ClassicInput, ClassicOutput, ClassicPlot, ClassicPolyline,
    flag, len, num, text,
};
use super::math::{js_max, js_min, source_values};
use super::shared::{
    SETTINGS, STYLE, bool_input, color_input, float_input, int_input, option_input, source_input,
    transp,
};

const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Session,
    Week,
    Month,
    Quarter,
    Year,
}

impl Anchor {
    /// An unknown name anchors to the session, like the original's fall-through.
    fn parse(name: &str) -> Self {
        match name {
            "Week" => Anchor::Week,
            "Month" => Anchor::Month,
            "Quarter" => Anchor::Quarter,
            "Year" => Anchor::Year,
            _ => Anchor::Session,
        }
    }
}

/// Year and month (1 to 12) of a day count since 1970-01-01 in the proleptic
/// Gregorian calendar (Howard Hinnant's civil-from-days algorithm).
fn civil_from_days(days: i64) -> (i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month)
}

/// UTC period key for an anchor (epoch day 0 was a Thursday; weeks start Monday).
fn period_key(anchor: Anchor, time: i64) -> i64 {
    let days = time.div_euclid(DAY_MS);
    match anchor {
        Anchor::Week => (days + 3).div_euclid(7),
        Anchor::Month => {
            let (year, month) = civil_from_days(days);
            year * 12 + (month - 1)
        }
        Anchor::Quarter => {
            let (year, month) = civil_from_days(days);
            year * 4 + (month - 1) / 3
        }
        Anchor::Year => civil_from_days(days).0,
        Anchor::Session => days,
    }
}

/// Median spacing between bars, in ms: the classics' only handle on the chart's timeframe.
fn bar_interval(bars: &[Ohlcv]) -> i64 {
    if bars.len() < 2 {
        return 0;
    }
    let mut gaps: Vec<i64> = bars.windows(2).map(|w| w[1].time - w[0].time).collect();
    gaps.sort_unstable();
    gaps[gaps.len() / 2]
}

fn nan_series(n: usize) -> Vec<f64> {
    vec![f64::NAN; n]
}

fn parabolic_sar() -> ClassicIndicatorSpec {
    let inputs = vec![
        float_input(
            "start",
            msg("indicator.parabolicSar.start", "Start"),
            0.02,
            0.0,
            1.0,
            0.01,
            Some(msg(
                "indicator.parabolicSar.start.tooltip",
                "Initial acceleration factor applied when a new trend begins.",
            )),
        ),
        float_input(
            "increment",
            msg("indicator.parabolicSar.increment", "Increment"),
            0.02,
            0.0,
            1.0,
            0.01,
            Some(msg(
                "indicator.parabolicSar.increment.tooltip",
                "Amount added to the acceleration factor each time the trend makes a new extreme point.",
            )),
        ),
        float_input(
            "maximum",
            msg("indicator.parabolicSar.maximum", "Maximum"),
            0.2,
            0.01,
            1.0,
            0.01,
            Some(msg(
                "indicator.parabolicSar.maximum.tooltip",
                "Upper cap on the acceleration factor.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.parabolicSar.bullColor", "Bullish"),
            "bullColor",
            Some(msg(
                "indicator.parabolicSar.bullColor.tooltip",
                "Color of the SAR crosses while they trail below price — uptrend, the stop sits under the market.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.parabolicSar.bearColor", "Bearish"),
            "bearColor",
            Some(msg(
                "indicator.parabolicSar.bearColor.tooltip",
                "Color of the SAR crosses while they sit above price — downtrend, the stop sits over the market.",
            )),
        ),
    ];
    ClassicIndicatorSpec::new(
        "parabolic-sar",
        msg("indicator.parabolicSar.title", "Parabolic SAR"),
        true,
        inputs,
        |bars, inputs| {
            let start = num(inputs, "start", 0.02);
            let inc = num(inputs, "increment", 0.02);
            let max = num(inputs, "maximum", 0.2);
            let n = bars.len();
            let mut values = nan_series(n);
            if n >= 2 {
                let mut long = bars[1].close >= bars[0].close;
                let mut sar = if long { bars[0].low } else { bars[0].high };
                let mut ep = if long { bars[0].high } else { bars[0].low };
                let mut af = start;
                for i in 1..n {
                    let b = &bars[i];
                    sar += af * (ep - sar);
                    // SAR may never poke inside the prior two bars' range.
                    if long {
                        let two_back = if i >= 2 {
                            bars[i - 2].low
                        } else {
                            bars[i - 1].low
                        };
                        sar = js_min(js_min(sar, bars[i - 1].low), two_back);
                    } else {
                        let two_back = if i >= 2 {
                            bars[i - 2].high
                        } else {
                            bars[i - 1].high
                        };
                        sar = js_max(js_max(sar, bars[i - 1].high), two_back);
                    }
                    let reversed = if long { b.low < sar } else { b.high > sar };
                    if reversed {
                        long = !long;
                        sar = ep;
                        ep = if long { b.high } else { b.low };
                        af = start;
                    } else if long && b.high > ep {
                        ep = b.high;
                        af = js_min(max, af + inc);
                    } else if !long && b.low < ep {
                        ep = b.low;
                        af = js_min(max, af + inc);
                    }
                    values[i] = sar;
                }
            }
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let colors: Vec<Option<String>> = values
                .iter()
                .zip(bars)
                .map(|(&x, b)| {
                    x.is_finite().then(|| {
                        if x < b.close {
                            bull.clone()
                        } else {
                            bear.clone()
                        }
                    })
                })
                .collect();
            ClassicOutput {
                plots: vec![
                    ClassicPlot::new(
                        "sar",
                        msg("indicator.parabolicSar.plot.sar", "SAR"),
                        values,
                        bull,
                    )
                    .kind(LineLikeKind::Cross)
                    .colors(colors),
                ],
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.parabolicSar.shortTitle", "SAR"))
}

const VWAP_ANCHORS: [&str; 5] = ["Session", "Week", "Month", "Quarter", "Year"];
const BANDS_STDEV: &str = "Standard Deviation";
const BANDS_PERCENT: &str = "Percentage";

struct VwapBand {
    show: &'static str,
    mult: &'static str,
    def_mult: f64,
    def_on: bool,
}

const VWAP_BANDS: [VwapBand; 3] = [
    VwapBand {
        show: "band1",
        mult: "band1Mult",
        def_mult: 1.0,
        def_on: true,
    },
    VwapBand {
        show: "band2",
        mult: "band2Mult",
        def_mult: 2.0,
        def_on: false,
    },
    VwapBand {
        show: "band3",
        mult: "band3Mult",
        def_mult: 3.0,
        def_on: false,
    },
];

fn vwap() -> ClassicIndicatorSpec {
    // The per-band text; `{n}` is the band's number (1 to 3).
    let band_text = msg("indicator.vwap.band", "Band #{n}");
    let band_tooltips = [
        msg("indicator.vwap.band1.tooltip", "Show the first band pair."),
        msg("indicator.vwap.band2.tooltip", "Show the second band pair."),
        msg("indicator.vwap.band3.tooltip", "Show the third band pair."),
    ];
    let mult_text = msg("indicator.vwap.bandMult", "Band #{n} Multiplier");
    let mult_tooltip = msg(
        "indicator.vwap.bandMult.tooltip",
        "Multiplier for this band pair, in standard deviations or percent depending on the bands mode.",
    );
    let upper_text = msg("indicator.vwap.plot.upperBand", "Upper Band #{n}");
    let lower_text = msg("indicator.vwap.plot.lowerBand", "Lower Band #{n}");

    let mut inputs = vec![
        option_input(
            "anchor",
            msg("indicator.vwap.anchor", "Anchor Period"),
            "Session",
            &VWAP_ANCHORS,
            Some(msg(
                "indicator.vwap.anchor.tooltip",
                "Period anchoring the average. VWAP and its bands reset on the first bar of each new session, week, month, quarter or year (UTC).",
            )),
        ),
        source_input(
            "HLC3",
            "source",
            msg("indicator.vwap.source", "Source"),
            Some(msg(
                "indicator.vwap.source.tooltip",
                "Price used in the volume weighted average. Typical price (HLC3) is the classic choice.",
            )),
        ),
        option_input(
            "bandsMode",
            msg("indicator.vwap.bandsMode", "Bands Mode"),
            BANDS_STDEV,
            &[BANDS_STDEV, BANDS_PERCENT],
            Some(msg(
                "indicator.vwap.bandsMode.tooltip",
                "Bands offset method: multiples of the volume weighted standard deviation of price around VWAP, or a percentage of the VWAP value.",
            )),
        ),
    ];
    for (i, b) in VWAP_BANDS.iter().enumerate() {
        inputs.push(bool_input(
            b.show,
            with_values(&band_text, [("n", (i + 1).into())]),
            b.def_on,
            SETTINGS,
            Some(band_tooltips[i].clone()),
        ));
        inputs.push(float_input(
            b.mult,
            with_values(&mult_text, [("n", (i + 1).into())]),
            b.def_mult,
            0.0,
            50.0,
            0.5,
            Some(mult_tooltip.clone()),
        ));
    }
    inputs.push(bool_input(
        "hideDwm",
        msg("indicator.vwap.hideDwm", "Hide VWAP on 1D or Above"),
        true,
        SETTINGS,
        Some(msg("indicator.vwap.hideDwm.tooltip", "Hide the VWAP and its bands on daily and higher timeframes, where an intraday anchored VWAP is not meaningful.")),
    ));
    inputs.push(color_input(
        BULLISH,
        msg("indicator.vwap.bullColor", "Bullish"),
        "bullColor",
        Some(msg("indicator.vwap.bullColor.tooltip", "VWAP line color while price closes above it, and the color of the lower (support) bands.")),
    ));
    inputs.push(color_input(
        BEARISH,
        msg("indicator.vwap.bearColor", "Bearish"),
        "bearColor",
        Some(msg("indicator.vwap.bearColor.tooltip", "VWAP line color while price closes below it, and the color of the upper (resistance) bands.")),
    ));
    inputs.push(color_input(
        NEUTRAL,
        msg("indicator.vwap.bandsColor", "Bands"),
        "bandsColor",
        Some(msg(
            "indicator.vwap.bandsColor.tooltip",
            "Deviation bands fill color.",
        )),
    ));
    inputs.push(bool_input(
        "fill",
        msg("indicator.vwap.fill", "Bands Fill"),
        true,
        STYLE,
        Some(msg(
            "indicator.vwap.fill.tooltip",
            "Fill the area between each visible band pair.",
        )),
    ));

    ClassicIndicatorSpec::new(
        "vwap",
        msg("indicator.vwap.title", "Volume Weighted Average Price"),
        true,
        inputs,
        move |bars, inputs| {
            struct Band {
                mult: f64,
                up: Vec<f64>,
                down: Vec<f64>,
            }
            let anchor = Anchor::parse(&text(inputs, "anchor", "Session"));
            let src = source_values(bars, &text(inputs, "source", "HLC3"));
            let n = bars.len();
            let hidden = flag(inputs, "hideDwm", true) && bar_interval(bars) >= DAY_MS;
            let mut values = nan_series(n);
            let mut bands: Vec<Band> = VWAP_BANDS
                .iter()
                .filter(|b| flag(inputs, b.show, b.def_on))
                .map(|b| Band {
                    mult: js_max(0.0, num(inputs, b.mult, b.def_mult)),
                    up: nan_series(n),
                    down: nan_series(n),
                })
                .collect();
            let percent_mode = text(inputs, "bandsMode", BANDS_STDEV) == BANDS_PERCENT;
            let mut period: Option<i64> = None;
            let (mut cum_pv, mut cum_pv2, mut cum_v) = (0.0_f64, 0.0_f64, 0.0_f64);
            if !hidden {
                for (i, b) in bars.iter().enumerate() {
                    let key = period_key(anchor, b.time);
                    if period != Some(key) {
                        period = Some(key);
                        cum_pv = 0.0;
                        cum_pv2 = 0.0;
                        cum_v = 0.0;
                    }
                    if let Some(v) = b.volume.filter(|v| v.is_finite() && *v > 0.0) {
                        let tp = src[i];
                        cum_pv += tp * v;
                        cum_pv2 += tp * tp * v;
                        cum_v += v;
                    }
                    if cum_v <= 0.0 {
                        continue;
                    }
                    let mean = cum_pv / cum_v;
                    // Variance clamped at 0: IEEE drift can dip `E[x²] − mean²` a hair under.
                    let offset = if percent_mode {
                        mean / 100.0
                    } else {
                        js_max(0.0, cum_pv2 / cum_v - mean * mean).sqrt()
                    };
                    values[i] = mean;
                    for band in &mut bands {
                        band.up[i] = mean + band.mult * offset;
                        band.down[i] = mean - band.mult * offset;
                    }
                }
            }
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let fill_color = transp(&text(inputs, "bandsColor", NEUTRAL), 95.0);
            let vwap_colors: Vec<Option<String>> = values
                .iter()
                .zip(bars)
                .map(|(&x, b)| {
                    x.is_finite().then(|| {
                        if b.close >= x {
                            bull.clone()
                        } else {
                            bear.clone()
                        }
                    })
                })
                .collect();
            let mut plots = vec![
                ClassicPlot::new(
                    "vwap",
                    msg("indicator.vwap.plot.vwap", "VWAP"),
                    values,
                    bull.clone(),
                )
                .colors(vwap_colors),
            ];
            let mut fills = Vec::new();
            let fill = flag(inputs, "fill", true);
            for (i, band) in bands.into_iter().enumerate() {
                plots.push(ClassicPlot::new(
                    format!("up{i}"),
                    with_values(&upper_text, [("n", (i + 1).into())]),
                    band.up,
                    transp(&bear, 40.0),
                ));
                plots.push(ClassicPlot::new(
                    format!("down{i}"),
                    with_values(&lower_text, [("n", (i + 1).into())]),
                    band.down,
                    transp(&bull, 40.0),
                ));
                if fill {
                    fills.push(ClassicBand::new(
                        format!("band{i}"),
                        format!("up{i}"),
                        format!("down{i}"),
                        fill_color.clone(),
                    ));
                }
            }
            ClassicOutput {
                plots,
                bands: fills,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.vwap.shortTitle", "VWAP"))
}

const PIVOT_TRADITIONAL: &str = "Traditional";
const PIVOT_FIBONACCI: &str = "Fibonacci";
const PIVOT_CAMARILLA: &str = "Camarilla";
const PIVOT_WOODIE: &str = "Woodie";
const PIVOT_AUTO: &str = "Auto";

#[derive(Clone, Copy)]
struct Agg {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

fn pivot_points() -> ClassicIndicatorSpec {
    let inputs = vec![
        option_input(
            "kind",
            msg("indicator.pivotPoints.kind", "Type"),
            PIVOT_TRADITIONAL,
            &[
                PIVOT_TRADITIONAL,
                PIVOT_FIBONACCI,
                PIVOT_CAMARILLA,
                PIVOT_WOODIE,
            ],
            Some(with_values(
                &msg(
                    "indicator.pivotPoints.kind.tooltip",
                    "Formula used to calculate the levels. Traditional is the classic floor-trader method; Fibonacci projects {level1, number}/{level2, number}/{level3, number}% of the prior range around the pivot; Camarilla scales the prior range around the prior close; Woodie gives extra weight to the period open.",
                ),
                [
                    ("level1", 38.2.into()),
                    ("level2", 61.8.into()),
                    ("level3", 100.0.into()),
                ],
            )),
        ),
        option_input(
            "anchor",
            msg("indicator.pivotPoints.anchor", "Pivots Timeframe"),
            PIVOT_AUTO,
            &[PIVOT_AUTO, "Daily", "Weekly", "Monthly", "Yearly"],
            Some(msg(
                "indicator.pivotPoints.anchor.tooltip",
                "Period the pivots are anchored to. Auto follows the chart's timeframe: daily pivots up to 15-minute bars, weekly on other intraday bars, monthly on daily bars, yearly above.",
            )),
        ),
        color_input(
            WARNING,
            msg("indicator.pivotPoints.pivotColor", "Pivot"),
            "pivotColor",
            Some(msg(
                "indicator.pivotPoints.pivotColor.tooltip",
                "Color of the central pivot (P) level.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.pivotPoints.resistanceColor", "Resistances"),
            "resistanceColor",
            Some(msg(
                "indicator.pivotPoints.resistanceColor.tooltip",
                "Color of the resistance (R) levels.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.pivotPoints.supportColor", "Supports"),
            "supportColor",
            Some(msg(
                "indicator.pivotPoints.supportColor.tooltip",
                "Color of the support (S) levels.",
            )),
        ),
    ];
    ClassicIndicatorSpec::new(
        "pivot-points",
        msg("indicator.pivotPoints.title", "Pivot Points Standard"),
        true,
        inputs,
        |bars, inputs| {
            let interval = bar_interval(bars);
            let requested = text(inputs, "anchor", PIVOT_AUTO);
            let auto = if interval <= 15 * 60_000 {
                Anchor::Session
            } else if interval < DAY_MS {
                Anchor::Week
            } else if interval <= DAY_MS {
                Anchor::Month
            } else {
                Anchor::Year
            };
            let anchor = match requested.as_str() {
                "Daily" => Anchor::Session,
                "Weekly" => Anchor::Week,
                "Monthly" => Anchor::Month,
                "Yearly" => Anchor::Year,
                _ => auto,
            };
            let kind = text(inputs, "kind", PIVOT_TRADITIONAL);
            // Aggregate each period's OHLC; a period's levels come from the PREVIOUS one.
            let mut aggs: HashMap<i64, Agg> = HashMap::new();
            for b in bars {
                aggs.entry(period_key(anchor, b.time))
                    .and_modify(|a| {
                        a.high = js_max(a.high, b.high);
                        a.low = js_min(a.low, b.low);
                        a.close = b.close;
                    })
                    .or_insert(Agg {
                        open: b.open,
                        high: b.high,
                        low: b.low,
                        close: b.close,
                    });
            }
            let n = bars.len();
            let (mut p_, mut r1, mut s1, mut r2, mut s2) = (
                nan_series(n),
                nan_series(n),
                nan_series(n),
                nan_series(n),
                nan_series(n),
            );
            let (mut r3, mut s3, mut r4, mut s4) =
                (nan_series(n), nan_series(n), nan_series(n), nan_series(n));
            let mut last_key: Option<i64> = None;
            for (i, bar) in bars.iter().enumerate() {
                let key = period_key(anchor, bar.time);
                let fresh = last_key != Some(key);
                last_key = Some(key);
                // A break on the period's first bar keeps each period's ladder a separate segment.
                if fresh {
                    continue;
                }
                let (Some(prev), Some(curr)) = (aggs.get(&(key - 1)), aggs.get(&key)) else {
                    continue;
                };
                let range = prev.high - prev.low;
                let traditional = (prev.high + prev.low + prev.close) / 3.0;
                let p = if kind == PIVOT_WOODIE {
                    (prev.high + prev.low + 2.0 * curr.open) / 4.0
                } else {
                    traditional
                };
                p_[i] = p;
                if kind == PIVOT_FIBONACCI {
                    r1[i] = traditional + 0.382 * range;
                    s1[i] = traditional - 0.382 * range;
                    r2[i] = traditional + 0.618 * range;
                    s2[i] = traditional - 0.618 * range;
                    r3[i] = traditional + range;
                    s3[i] = traditional - range;
                } else if kind == PIVOT_CAMARILLA {
                    let cam = 1.1 * range;
                    r1[i] = prev.close + cam / 12.0;
                    s1[i] = prev.close - cam / 12.0;
                    r2[i] = prev.close + cam / 6.0;
                    s2[i] = prev.close - cam / 6.0;
                    r3[i] = prev.close + cam / 4.0;
                    s3[i] = prev.close - cam / 4.0;
                    r4[i] = prev.close + cam / 2.0;
                    s4[i] = prev.close - cam / 2.0;
                } else {
                    // Traditional and Woodie share the R1/S1/R2/S2 shape around their own pivot.
                    r1[i] = 2.0 * p - prev.low;
                    s1[i] = 2.0 * p - prev.high;
                    r2[i] = p + range;
                    s2[i] = p - range;
                    if kind != PIVOT_WOODIE {
                        r3[i] = prev.high + 2.0 * (p - prev.low);
                        s3[i] = prev.low - 2.0 * (prev.high - p);
                    }
                }
            }
            let resistance = text(inputs, "resistanceColor", BEARISH);
            let support = text(inputs, "supportColor", BULLISH);
            let mut plots = vec![
                ClassicPlot::new("p", "P", p_, text(inputs, "pivotColor", WARNING)),
                ClassicPlot::new("r1", "R1", r1, resistance.clone()),
                ClassicPlot::new("s1", "S1", s1, support.clone()),
                ClassicPlot::new("r2", "R2", r2, resistance.clone()),
                ClassicPlot::new("s2", "S2", s2, support.clone()),
            ];
            if kind != PIVOT_WOODIE {
                plots.push(ClassicPlot::new("r3", "R3", r3, resistance.clone()));
                plots.push(ClassicPlot::new("s3", "S3", s3, support.clone()));
            }
            if kind == PIVOT_CAMARILLA {
                plots.push(ClassicPlot::new("r4", "R4", r4, resistance));
                plots.push(ClassicPlot::new("s4", "S4", s4, support));
            }
            ClassicOutput {
                plots,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.pivotPoints.shortTitle", "Pivots"))
}

fn fifty_two_week() -> ClassicIndicatorSpec {
    let inputs = vec![
        int_input(
            "weeks",
            msg("indicator.n52WeekHighLow.weeks", "Weeks"),
            52.0,
            1.0,
            520.0,
            Some(msg(
                "indicator.n52WeekHighLow.weeks.tooltip",
                "Length of the rolling window, in weeks.",
            )),
        ),
        bool_input(
            "showAllTime",
            msg(
                "indicator.n52WeekHighLow.showAllTime",
                "Show All-Time High/Low",
            ),
            false,
            SETTINGS,
            Some(msg(
                "indicator.n52WeekHighLow.showAllTime.tooltip",
                "Additionally display the all-time high and low, tracked over the symbol's full loaded history.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.n52WeekHighLow.highColor", "52-Week High"),
            "highColor",
            Some(msg(
                "indicator.n52WeekHighLow.highColor.tooltip",
                "Color of the 52-week high level.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.n52WeekHighLow.lowColor", "52-Week Low"),
            "lowColor",
            Some(msg(
                "indicator.n52WeekHighLow.lowColor.tooltip",
                "Color of the 52-week low level.",
            )),
        ),
        color_input(
            NEUTRAL,
            msg("indicator.n52WeekHighLow.allTimeColor", "All-Time High/Low"),
            "allTimeColor",
            Some(msg(
                "indicator.n52WeekHighLow.allTimeColor.tooltip",
                "Color of the all-time high and all-time low levels.",
            )),
        ),
    ];
    ClassicIndicatorSpec::new(
        "52-week-high-low",
        msg("indicator.n52WeekHighLow.title", "52-Week High/Low"),
        true,
        inputs,
        |bars, inputs| {
            let span = num(inputs, "weeks", 52.0) * 7.0 * DAY_MS as f64;
            let n = bars.len();
            let (mut hi, mut lo, mut ath, mut atl) =
                (nan_series(n), nan_series(n), nan_series(n), nan_series(n));
            // Two-pointer sliding window over the time span, monotonic deques for the extremes.
            let mut max_idx: VecDeque<usize> = VecDeque::new();
            let mut min_idx: VecDeque<usize> = VecDeque::new();
            let mut from = 0;
            let mut run_high = f64::NEG_INFINITY;
            let mut run_low = f64::INFINITY;
            for (i, b) in bars.iter().enumerate() {
                while max_idx.back().is_some_and(|&j| bars[j].high <= b.high) {
                    max_idx.pop_back();
                }
                max_idx.push_back(i);
                while min_idx.back().is_some_and(|&j| bars[j].low >= b.low) {
                    min_idx.pop_back();
                }
                min_idx.push_back(i);
                // `from < i` only guards a nonsensical negative span; a real one never passes bar i.
                while from < i && (bars[from].time as f64) < b.time as f64 - span {
                    from += 1;
                }
                while max_idx.front().is_some_and(|&j| j < from) {
                    max_idx.pop_front();
                }
                while min_idx.front().is_some_and(|&j| j < from) {
                    min_idx.pop_front();
                }
                if let (Some(&h), Some(&l)) = (max_idx.front(), min_idx.front()) {
                    hi[i] = bars[h].high;
                    lo[i] = bars[l].low;
                }
                run_high = js_max(run_high, b.high);
                run_low = js_min(run_low, b.low);
                ath[i] = run_high;
                atl[i] = run_low;
            }
            let mut plots = vec![
                ClassicPlot::new(
                    "high",
                    "52 Week High",
                    hi,
                    text(inputs, "highColor", BULLISH),
                )
                .display_title(msg("indicator.n52WeekHighLow.plot.high", "52-Week High"))
                .kind(LineLikeKind::Step)
                .width(2.0),
                ClassicPlot::new("low", "52 Week Low", lo, text(inputs, "lowColor", BEARISH))
                    .display_title(msg("indicator.n52WeekHighLow.plot.low", "52-Week Low"))
                    .kind(LineLikeKind::Step)
                    .width(2.0),
            ];
            if flag(inputs, "showAllTime", false) {
                let ink = text(inputs, "allTimeColor", NEUTRAL);
                plots.push(
                    ClassicPlot::new(
                        "ath",
                        msg("indicator.n52WeekHighLow.plot.ath", "All-Time High"),
                        ath,
                        ink.clone(),
                    )
                    .kind(LineLikeKind::Step),
                );
                plots.push(
                    ClassicPlot::new(
                        "atl",
                        msg("indicator.n52WeekHighLow.plot.atl", "All-Time Low"),
                        atl,
                        ink,
                    )
                    .kind(LineLikeKind::Step),
                );
            }
            ClassicOutput {
                plots,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.n52WeekHighLow.shortTitle", "52W H/L"))
}

#[derive(Clone, Copy)]
struct Pivot {
    i: usize,
    price: f64,
}

fn zigzag() -> ClassicIndicatorSpec {
    let mut line_width = ClassicInput::new(
        "lineWidth",
        msg("indicator.zigzag.lineWidth", "Width"),
        InputType::Int,
        2.0,
    )
    .group(STYLE)
    .tooltip(msg(
        "indicator.zigzag.lineWidth.tooltip",
        "Width of the zigzag segments.",
    ));
    line_width.min = Some(1.0);
    line_width.max = Some(10.0);
    line_width.step = Some(1.0);
    let inputs = vec![
        float_input(
            "deviation",
            msg("indicator.zigzag.deviation", "Deviation (%)"),
            5.0,
            0.01,
            100.0,
            0.1,
            Some(msg(
                "indicator.zigzag.deviation.tooltip",
                "Minimum reversal from the leg extreme, as a percentage of that extreme, required to confirm a new pivot.",
            )),
        ),
        int_input(
            "depth",
            msg("indicator.zigzag.depth", "Depth"),
            10.0,
            2.0,
            500.0,
            Some(msg(
                "indicator.zigzag.depth.tooltip",
                "Minimum number of bars required between two consecutive pivots.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.zigzag.bullColor", "Up Leg"),
            "bullColor",
            Some(msg(
                "indicator.zigzag.bullColor.tooltip",
                "Color of rising zigzag segments.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.zigzag.bearColor", "Down Leg"),
            "bearColor",
            Some(msg(
                "indicator.zigzag.bearColor.tooltip",
                "Color of falling zigzag segments.",
            )),
        ),
        line_width,
    ];
    ClassicIndicatorSpec::new(
        "zigzag",
        msg("indicator.zigzag.title", "ZigZag"),
        true,
        inputs,
        |bars, inputs| {
            let dev = num(inputs, "deviation", 5.0) / 100.0;
            let depth = num(inputs, "depth", 10.0);
            let mut pivots: Vec<Pivot> = Vec::new();
            if bars.len() > 1 {
                // Walk the bars tracking the running extreme; a retrace beyond the deviation
                // (and at least `depth` bars past the last pivot) confirms the extreme as a pivot.
                let mut up = bars[1].close >= bars[0].close;
                let mut ext = Pivot {
                    i: 0,
                    price: if up { bars[0].high } else { bars[0].low },
                };
                for (i, b) in bars.iter().enumerate().skip(1) {
                    let since_last = |ext: &Pivot, pivots: &[Pivot]| {
                        ext.i as f64 - pivots.last().map_or(-depth, |p| p.i as f64) >= depth
                    };
                    if up {
                        if b.high >= ext.price {
                            ext = Pivot { i, price: b.high };
                        } else if b.low <= ext.price * (1.0 - dev) && since_last(&ext, &pivots) {
                            pivots.push(ext);
                            up = false;
                            ext = Pivot { i, price: b.low };
                        }
                    } else if b.low <= ext.price {
                        ext = Pivot { i, price: b.low };
                    } else if b.high >= ext.price * (1.0 + dev) && since_last(&ext, &pivots) {
                        pivots.push(ext);
                        up = true;
                        ext = Pivot { i, price: b.high };
                    }
                }
                pivots.push(ext); // the provisional last leg
            }
            let bull = text(inputs, "bullColor", BULLISH);
            let bear = text(inputs, "bearColor", BEARISH);
            let width = num(inputs, "lineWidth", 2.0);
            // One polyline per leg, so each segment can carry its own direction ink.
            let polylines = pivots
                .windows(2)
                .enumerate()
                .map(|(k, w)| {
                    let (a, b) = (w[0], w[1]);
                    ClassicPolyline {
                        key: format!("leg{}", k + 1),
                        points: vec![
                            PolylinePoint {
                                xloc: DrawingXLoc::BarTime,
                                x: bars[a.i].time as f64,
                                price: a.price,
                            },
                            PolylinePoint {
                                xloc: DrawingXLoc::BarTime,
                                x: bars[b.i].time as f64,
                                price: b.price,
                            },
                        ],
                        curved: false,
                        closed: false,
                        line_color: Some(if b.price >= a.price {
                            bull.clone()
                        } else {
                            bear.clone()
                        }),
                        fill_color: None,
                        line_width: width,
                        line_style: LineStyle::Solid,
                        arrow_left: false,
                        arrow_right: false,
                        overlay: false,
                    }
                })
                .collect();
            ClassicOutput {
                polylines,
                ..Default::default()
            }
        },
    )
}

fn williams_fractal() -> ClassicIndicatorSpec {
    let inputs = vec![
        int_input(
            "periods",
            msg("indicator.williamsFractal.periods", "Periods"),
            2.0,
            2.0,
            50.0,
            Some(msg(
                "indicator.williamsFractal.periods.tooltip",
                "Bars required on each side of the candidate bar. 2 is Bill Williams' classic 5-bar fractal; a fractal is confirmed only once this many later bars have closed.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.williamsFractal.upColor", "Up Fractal"),
            "upColor",
            Some(msg(
                "indicator.williamsFractal.upColor.tooltip",
                "Color of the up fractal marker (swing high), drawn above the fractal bar.",
            )),
        ),
        color_input(
            BULLISH,
            msg("indicator.williamsFractal.downColor", "Down Fractal"),
            "downColor",
            Some(msg(
                "indicator.williamsFractal.downColor.tooltip",
                "Color of the down fractal marker (swing low), drawn below the fractal bar.",
            )),
        ),
    ];
    ClassicIndicatorSpec::new(
        "williams-fractal",
        msg("indicator.williamsFractal.title", "Williams Fractal"),
        true,
        inputs,
        |bars, inputs| {
            let p = len(inputs, "periods", 2);
            let up = text(inputs, "upColor", BEARISH);
            let down = text(inputs, "downColor", BULLISH);
            // The candidate must STRICTLY exceed every neighbour, so a tie never prints a fractal.
            let is_extreme =
                |i: usize, pick: fn(&Ohlcv) -> f64, better: fn(f64, f64) -> bool| -> bool {
                    let v = pick(&bars[i]);
                    (i - p..=i + p).all(|k| k == i || better(v, pick(&bars[k])))
                };
            let mut markers = Vec::new();
            for (i, bar) in bars
                .iter()
                .enumerate()
                .take(bars.len().saturating_sub(p))
                .skip(p)
            {
                if is_extreme(i, |b| b.high, |a, b| a > b) {
                    markers.push(MarkerPoint {
                        time: bar.time,
                        position: MarkerPosition::AboveBar,
                        shape: "triangleup".to_string(),
                        color: up.clone(),
                        text: None,
                        size: None,
                    });
                }
                if is_extreme(i, |b| b.low, |a, b| a < b) {
                    markers.push(MarkerPoint {
                        time: bar.time,
                        position: MarkerPosition::BelowBar,
                        shape: "triangledown".to_string(),
                        color: down.clone(),
                        text: None,
                        size: None,
                    });
                }
            }
            ClassicOutput {
                markers,
                ..Default::default()
            }
        },
    )
    .short_title(msg("indicator.williamsFractal.shortTitle", "Fractals"))
}

pub fn overlay_specs() -> Vec<ClassicIndicatorSpec> {
    vec![
        parabolic_sar(),
        vwap(),
        pivot_points(),
        fifty_two_week(),
        zigzag(),
        williams_fractal(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::source_text;
    use crate::indicators::classics::define::{InputLabels, classic_native_output, localize_input};
    use crate::model::{InputValue, Inputs, SeriesSpec};

    fn spec(indicator_type: &str) -> ClassicIndicatorSpec {
        overlay_specs()
            .into_iter()
            .find(|s| s.indicator_type == indicator_type)
            .expect("spec exists")
    }

    fn bar(close: f64, i: i64, volume: Option<f64>) -> Ohlcv {
        Ohlcv::new(i * 60_000, close, close + 1.0, close - 1.0, close, volume)
    }

    fn flat(time: i64, price: f64, volume: f64) -> Ohlcv {
        Ohlcv::new(time, price, price, price, price, Some(volume))
    }

    /// Run a spec through the adapter with exactly the given inputs (no defaults mixed in).
    fn run(
        spec: &ClassicIndicatorSpec,
        bars: &[Ohlcv],
        given: &[(&str, InputValue)],
    ) -> crate::indicators::NativeIndicatorOutput {
        let inputs: Inputs = given
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        classic_native_output(spec, bars, &inputs)
    }

    fn values(series: &SeriesSpec) -> Vec<Option<f64>> {
        series
            .points()
            .expect("line-like series")
            .iter()
            .map(|p| p.value)
            .collect()
    }

    fn titles(out: &crate::indicators::NativeIndicatorOutput) -> Vec<String> {
        out.series.iter().map(|s| s.title.clone()).collect()
    }

    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn utc_ms(y: i64, m: i64, d: i64, h: i64) -> i64 {
        days_from_civil(y, m, d) * DAY_MS + h * 3_600_000
    }

    fn close_to(actual: Option<f64>, expected: f64) {
        let a = actual.expect("a value");
        assert!((a - expected).abs() < 1e-10, "{a} vs {expected}");
    }

    #[test]
    fn civil_calendar_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1));
        assert_eq!(civil_from_days(-1), (1969, 12));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29) + 1), (2024, 3));
        assert_eq!(civil_from_days(days_from_civil(2023, 2, 28) + 1), (2023, 3));
        assert_eq!(civil_from_days(days_from_civil(2000, 12, 31)), (2000, 12));
        assert_eq!(civil_from_days(days_from_civil(1900, 3, 1) - 1), (1900, 2));
        assert_eq!(civil_from_days(days_from_civil(2025, 1, 1)), (2025, 1));
    }

    #[test]
    fn weeks_start_on_monday_and_epoch_day_zero_is_a_thursday() {
        // 1970-01-01 is a Thursday: days 0..=3 (Thu..Sun) share a week, Monday day 4 starts the next.
        let key = |days: i64| period_key(Anchor::Week, days * DAY_MS);
        assert_eq!(key(0), key(3));
        assert_ne!(key(3), key(4));
        assert_eq!(key(4), key(10));
        assert_ne!(key(10), key(11));
        // The Monday before the epoch (1969-12-29) shares the Thursday's week.
        assert_eq!(key(-3), key(0));
        assert_ne!(key(-4), key(-3));
        // 2024-01-01 was a Monday.
        let monday = days_from_civil(2024, 1, 1);
        assert_ne!(key(monday - 1), key(monday));
        assert_eq!(key(monday), key(monday + 6));
    }

    #[test]
    fn quarters_and_years_roll_over_at_utc_boundaries() {
        let q = |ms: i64| period_key(Anchor::Quarter, ms);
        assert_ne!(q(utc_ms(2024, 3, 31, 23)), q(utc_ms(2024, 4, 1, 0)));
        assert_eq!(q(utc_ms(2024, 1, 1, 0)), q(utc_ms(2024, 3, 31, 23)));
        let y = |ms: i64| period_key(Anchor::Year, ms);
        assert_ne!(y(utc_ms(2024, 12, 31, 23)), y(utc_ms(2025, 1, 1, 0)));
        let m = |ms: i64| period_key(Anchor::Month, ms);
        assert_eq!(m(utc_ms(2024, 2, 1, 0)), m(utc_ms(2024, 2, 29, 23)));
        assert_ne!(m(utc_ms(2024, 2, 29, 23)), m(utc_ms(2024, 3, 1, 0)));
        // Negative timestamps floor toward the earlier day.
        assert_eq!(period_key(Anchor::Session, -1), -1);
        assert_eq!(period_key(Anchor::Year, -1), 1969);
    }

    #[test]
    fn bar_interval_is_the_median_gap() {
        assert_eq!(bar_interval(&[]), 0);
        assert_eq!(bar_interval(&[bar(1.0, 0, None)]), 0);
        let bars: Vec<Ohlcv> = [0, 1, 2, 3, 10]
            .iter()
            .map(|&i| bar(1.0, i, None))
            .collect();
        assert_eq!(bar_interval(&bars), 60_000);
    }

    #[test]
    fn types_and_short_titles_follow_the_library() {
        let expected = [
            ("parabolic-sar", "SAR"),
            ("vwap", "VWAP"),
            ("pivot-points", "Pivots"),
            ("52-week-high-low", "52W H/L"),
            ("zigzag", "ZigZag"),
            ("williams-fractal", "Fractals"),
        ];
        let specs = overlay_specs();
        assert_eq!(
            specs.iter().map(|s| s.indicator_type).collect::<Vec<_>>(),
            expected.iter().map(|(t, _)| *t).collect::<Vec<_>>()
        );
        for (s, (_, short)) in specs.iter().zip(expected) {
            assert_eq!(
                source_text(s.short_title.as_ref().unwrap_or(&s.title)),
                short,
                "{}",
                s.indicator_type
            );
        }
    }

    #[test]
    fn every_spec_computes_on_synthetic_bars_with_default_inputs() {
        let bars: Vec<Ohlcv> = (0..400)
            .map(|i| {
                let c = 100.0 + (i as f64 / 7.0).sin() * 12.0 + (i as f64 / 23.0).cos() * 5.0;
                Ohlcv::new(
                    i * 3_600_000,
                    c - 0.5,
                    c + 1.5,
                    c - 1.5,
                    c,
                    Some(1000.0 + (i % 17) as f64 * 10.0),
                )
            })
            .collect();
        for s in overlay_specs() {
            let defaults = s.default_inputs();
            let out = s.compute(&bars, &defaults);
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    bars.len(),
                    "{}:{}",
                    s.indicator_type,
                    plot.key
                );
                if let Some(colors) = &plot.colors {
                    assert_eq!(
                        colors.len(),
                        bars.len(),
                        "{}:{}",
                        s.indicator_type,
                        plot.key
                    );
                }
            }
            let native = classic_native_output(&s, &bars, &defaults);
            for series in &native.series {
                if let Some(points) = series.points() {
                    assert_eq!(points.len(), bars.len());
                }
            }
        }
        let zig = spec("zigzag").compute(&bars, &spec("zigzag").default_inputs());
        assert!(!zig.polylines.is_empty());
        let frac =
            spec("williams-fractal").compute(&bars, &spec("williams-fractal").default_inputs());
        assert!(!frac.markers.is_empty());
        let sar = spec("parabolic-sar").compute(&bars, &spec("parabolic-sar").default_inputs());
        assert!(sar.plots[0].values.iter().skip(1).all(|v| v.is_finite()));
    }

    #[test]
    fn short_inputs_do_not_panic() {
        for s in overlay_specs() {
            for n in 0..4 {
                let bars: Vec<Ohlcv> = (0..n)
                    .map(|i| bar(100.0 + i as f64, i, Some(1.0)))
                    .collect();
                let out = s.compute(&bars, &s.default_inputs());
                for plot in &out.plots {
                    assert_eq!(plot.values.len(), bars.len());
                }
            }
        }
    }

    #[test]
    fn vwap_anchors_accumulation_to_the_utc_day() {
        let s = spec("vwap");
        let bars = [
            flat(0, 10.0, 100.0),
            flat(60_000, 20.0, 100.0),
            flat(DAY_MS, 30.0, 100.0),
        ];
        let out = run(
            &s,
            &bars,
            &[("anchor", "Session".into()), ("source", "Close".into())],
        );
        let points = values(&out.series[0]);
        close_to(points[1], 15.0);
        close_to(points[2], 30.0); // reset at the new day
    }

    #[test]
    fn vwap_resets_on_utc_quarter_and_year_boundaries() {
        let s = spec("vwap");
        let bars = [
            flat(utc_ms(2024, 2, 10, 0), 10.0, 1.0),
            flat(utc_ms(2024, 3, 31, 23), 20.0, 1.0),
            flat(utc_ms(2024, 4, 1, 0), 30.0, 1.0),
            flat(utc_ms(2025, 1, 1, 0), 40.0, 1.0),
        ];
        // These bars sit days apart, so the daily-and-above guard has to be lifted first.
        let quarter = run(
            &s,
            &bars,
            &[
                ("anchor", "Quarter".into()),
                ("source", "Close".into()),
                ("hideDwm", false.into()),
            ],
        );
        assert_eq!(
            values(&quarter.series[0]),
            vec![Some(10.0), Some(15.0), Some(30.0), Some(40.0)]
        );
        let year = run(
            &s,
            &bars,
            &[
                ("anchor", "Year".into()),
                ("source", "Close".into()),
                ("hideDwm", false.into()),
            ],
        );
        assert_eq!(
            values(&year.series[0]),
            vec![Some(10.0), Some(15.0), Some(20.0), Some(40.0)]
        );
    }

    #[test]
    fn vwap_blanks_on_daily_and_higher_timeframes_unless_the_guard_is_off() {
        let s = spec("vwap");
        let daily: Vec<Ohlcv> = (0..5).map(|i| flat(i * DAY_MS, 10.0, 1.0)).collect();
        let guarded = run(&s, &daily, &[]);
        assert!(values(&guarded.series[0]).iter().all(Option::is_none));
        let open = run(&s, &daily, &[("hideDwm", false.into())]);
        assert!(values(&open.series[0]).iter().all(Option::is_some));
    }

    #[test]
    fn vwap_draws_bands_per_enabled_pair_in_deviations_or_percent_with_optional_fill() {
        let s = spec("vwap");
        // Flat bars make every source = price: vwap of (100x1, 106x2) = 104, sigma = sqrt(8).
        let bars = [flat(0, 100.0, 1.0), flat(60_000, 106.0, 2.0)];
        let out = run(
            &s,
            &bars,
            &[
                ("band1", true.into()),
                ("band1Mult", 1.0.into()),
                ("band2", false.into()),
                ("band3", true.into()),
                ("band3Mult", 2.5.into()),
            ],
        );
        assert_eq!(
            titles(&out),
            vec![
                "VWAP",
                "Upper Band #1",
                "Lower Band #1",
                "Upper Band #2",
                "Lower Band #2"
            ]
        );
        let sd = 8.0_f64.sqrt();
        close_to(values(&out.series[1])[1], 104.0 + sd);
        close_to(values(&out.series[4])[1], 104.0 - 2.5 * sd);

        // Percentage mode offsets by a share of the VWAP value instead of its deviation.
        let percent = run(
            &s,
            &bars,
            &[
                ("bandsMode", "Percentage".into()),
                ("band1Mult", 2.0.into()),
            ],
        );
        close_to(values(&percent.series[1])[1], 104.0 * 1.02);

        // Upper bands lean bearish, lower bands bullish, both at 40% transparency.
        let ink = |series: &SeriesSpec| series.line_style().expect("line-like").color.clone();
        assert_eq!(ink(&out.series[1]), "#f2364599");
        assert_eq!(ink(&out.series[2]), "#08998199");

        // Only band 1 is on by default; each visible pair gets one fill, and the toggle kills them all.
        assert_eq!(run(&s, &bars, &[]).series.len(), 3);
        assert_eq!(out.fills.len(), 2);
        assert_eq!(run(&s, &bars, &[("fill", false.into())]).fills.len(), 0);
        assert_eq!(out.fills[0].color.as_deref(), Some("#787b860d"));
    }

    #[test]
    fn vwap_reads_its_source_through_the_shared_vocabulary() {
        let s = spec("vwap");
        let source = s
            .inputs
            .iter()
            .find(|i| i.key == "source")
            .expect("source input");
        assert_eq!(
            source.options.as_deref().map(<[String]>::to_vec),
            Some(
                [
                    "Close", "Open", "High", "Low", "HL2", "HLC3", "OHLC4", "HLCC4"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        let bars = [Ohlcv::new(0, 1.0, 9.0, 3.0, 5.0, Some(1.0))];
        assert_eq!(
            values(&run(&s, &bars, &[("source", "High".into())]).series[0])[0],
            Some(9.0)
        );
        assert_eq!(
            values(&run(&s, &bars, &[("source", "HL2".into())]).series[0])[0],
            Some(6.0)
        );
    }

    #[test]
    fn vwap_declares_band_toggles_in_settings_and_its_inks_in_a_trailing_style_group() {
        let s = spec("vwap");
        let anchor = s
            .inputs
            .iter()
            .find(|i| i.key == "anchor")
            .expect("anchor input");
        assert_eq!(
            anchor.options.as_deref().map(<[String]>::to_vec),
            Some(
                ["Session", "Week", "Month", "Quarter", "Year"]
                    .map(String::from)
                    .to_vec()
            )
        );
        for n in 1..=3 {
            let on = s
                .inputs
                .iter()
                .find(|i| i.key == format!("band{n}"))
                .expect("band toggle");
            let mult = s
                .inputs
                .iter()
                .find(|i| i.key == format!("band{n}Mult"))
                .expect("band multiplier");
            assert_eq!(on.input_type, InputType::Bool);
            assert_eq!(on.group.as_deref(), Some("Settings"));
            assert_eq!(mult.input_type, InputType::Float);
            assert_eq!(mult.group.as_deref(), Some("Settings"));
        }
        let styled: Vec<&str> = s
            .inputs
            .iter()
            .filter(|i| i.group.as_deref() == Some("Style"))
            .map(|i| i.key.as_str())
            .collect();
        assert_eq!(styled, vec!["bullColor", "bearColor", "bandsColor", "fill"]);
        let tail: Vec<&str> = s.inputs[s.inputs.len() - styled.len()..]
            .iter()
            .map(|i| i.key.as_str())
            .collect();
        assert_eq!(tail, styled);
        let keys: Vec<&str> = s.inputs.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            &keys[..9],
            [
                "anchor",
                "source",
                "bandsMode",
                "band1",
                "band1Mult",
                "band2",
                "band2Mult",
                "band3",
                "band3Mult"
            ]
        );
        assert_eq!(source_text(&s.inputs[3].title), "Band #1");
        assert_eq!(source_text(&s.inputs[8].title), "Band #3 Multiplier");
    }

    #[test]
    fn pivot_points_name_the_fibonacci_ratios_in_the_tooltip_and_keep_english_option_values() {
        let s = spec("pivot-points");
        let kind = s
            .inputs
            .iter()
            .find(|i| i.key == "kind")
            .expect("kind input");
        let schema = localize_input(kind, &InputLabels::default());
        assert!(schema.tooltip.as_deref().is_some_and(|t| t.contains("Fibonacci projects 38.2/61.8/100% of the prior range")));
        assert_eq!(
            schema.options.as_deref(),
            Some(&["Traditional", "Fibonacci", "Camarilla", "Woodie"].map(String::from)[..])
        );
        assert!(schema.display.is_some_and(|d| d.options.is_none()));
    }

    #[test]
    fn pivot_points_derive_each_period_ladder_from_the_previous_period() {
        let s = spec("pivot-points");
        // Two UTC days of hourly bars; day 1 spans high 110 / low 90 / close 105.
        let mut bars = Vec::new();
        for h in 0..24 {
            let (hi, lo, close) = if h == 23 {
                (100.0, 95.0, 105.0)
            } else if h == 0 {
                (110.0, 90.0, 100.0)
            } else {
                (102.0, 98.0, 100.0)
            };
            bars.push(Ohlcv::new(h * 3_600_000, 100.0, hi, lo, close, Some(1.0)));
        }
        for h in 0..24 {
            bars.push(Ohlcv::new(
                DAY_MS + h * 3_600_000,
                104.0,
                106.0,
                102.0,
                104.0,
                Some(1.0),
            ));
        }
        let out = run(&s, &bars, &[("anchor", "Daily".into())]);
        assert_eq!(titles(&out), vec!["P", "R1", "S1", "R2", "S2", "R3", "S3"]);
        let p = values(&out.series[0]);
        assert!(p[..25].iter().all(Option::is_none));
        close_to(p[25], (110.0 + 90.0 + 105.0) / 3.0);

        let woodie = run(
            &s,
            &bars,
            &[("anchor", "Daily".into()), ("kind", "Woodie".into())],
        );
        assert_eq!(titles(&woodie), vec!["P", "R1", "S1", "R2", "S2"]);
        close_to(
            values(&woodie.series[0])[25],
            (110.0 + 90.0 + 2.0 * 104.0) / 4.0,
        );

        let camarilla = run(
            &s,
            &bars,
            &[("anchor", "Daily".into()), ("kind", "Camarilla".into())],
        );
        assert_eq!(titles(&camarilla).len(), 9);
        close_to(values(&camarilla.series[1])[25], 105.0 + 1.1 * 20.0 / 12.0);

        let fib = run(
            &s,
            &bars,
            &[("anchor", "Daily".into()), ("kind", "Fibonacci".into())],
        );
        close_to(
            values(&fib.series[1])[25],
            (110.0 + 90.0 + 105.0) / 3.0 + 0.382 * 20.0,
        );
    }

    #[test]
    fn parabolic_sar_flips_on_a_reversal_and_colors_by_side_of_price() {
        let s = spec("parabolic-sar");
        let mut closes: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        closes.extend((0..30).map(|i| 129.0 - i as f64 * 2.0));
        let bars: Vec<Ohlcv> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| bar(c, i as i64, None))
            .collect();
        let out = s.compute(&bars, &s.default_inputs());
        let plot = &out.plots[0];
        assert_eq!(plot.kind, LineLikeKind::Cross);
        assert!(plot.values[0].is_nan());
        let colors = plot.colors.as_ref().expect("per-bar colors");
        assert_eq!(colors[10].as_deref(), Some(BULLISH));
        assert_eq!(colors[55].as_deref(), Some(BEARISH));
        assert!(plot.values[10] < bars[10].close && plot.values[55] > bars[55].close);
    }

    #[test]
    fn fifty_two_week_tracks_a_time_window_and_the_running_extremes() {
        let s = spec("52-week-high-low");
        let bars: Vec<Ohlcv> = (0..10)
            .map(|i| {
                Ohlcv::new(
                    i * 7 * DAY_MS,
                    10.0,
                    20.0 - i as f64,
                    5.0 + i as f64,
                    10.0,
                    None,
                )
            })
            .collect();
        let out = run(
            &s,
            &bars,
            &[("weeks", 3.0.into()), ("showAllTime", true.into())],
        );
        assert_eq!(
            titles(&out),
            vec![
                "52 Week High",
                "52 Week Low",
                "All-Time High",
                "All-Time Low"
            ]
        );
        assert_eq!(
            out.series[0]
                .display_title
                .as_ref()
                .map(source_text)
                .as_deref(),
            Some("52-Week High")
        );
        // A 3-week window keeps bars i-3..=i: both extremes sit on the oldest bar in the window.
        assert_eq!(values(&out.series[0])[9], Some(20.0 - 6.0));
        assert_eq!(values(&out.series[1])[9], Some(5.0 + 6.0));
        assert_eq!(values(&out.series[2])[9], Some(20.0));
        assert_eq!(values(&out.series[3])[9], Some(5.0));
        assert_eq!(run(&s, &bars, &[]).series.len(), 2);
    }

    #[test]
    fn zigzag_connects_confirmed_swings_with_direction_inked_legs() {
        let s = spec("zigzag");
        let mut closes: Vec<f64> = (0..20).map(|i| 100.0 + i as f64 * 2.0).collect();
        closes.extend((0..20).map(|i| 138.0 - i as f64 * 2.0));
        closes.extend((0..20).map(|i| 100.0 + i as f64 * 2.0));
        let bars: Vec<Ohlcv> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| bar(c, i as i64, None))
            .collect();
        let out = s.compute(&bars, &s.default_inputs());
        // The first confirmed pivot is the peak, so the legs are peak -> trough -> provisional end.
        assert_eq!(out.polylines.len(), 2);
        assert_eq!(out.polylines[0].key, "leg1");
        assert_eq!(out.polylines[0].line_color.as_deref(), Some(BEARISH));
        assert_eq!(out.polylines[1].line_color.as_deref(), Some(BULLISH));
        assert!(
            out.polylines
                .iter()
                .all(|p| p.points.len() == 2 && p.line_width == 2.0)
        );
        assert!(out.plots.is_empty());
    }

    #[test]
    fn williams_fractal_needs_a_strict_extreme_on_both_sides() {
        let s = spec("williams-fractal");
        let highs = [1.0, 2.0, 5.0, 2.0, 1.0, 2.0, 3.0, 3.0, 1.0, 0.5];
        let bars: Vec<Ohlcv> = highs
            .iter()
            .enumerate()
            .map(|(i, &h)| Ohlcv::new(i as i64 * 60_000, h, h, h - 1.0, h, None))
            .collect();
        let out = s.compute(&bars, &s.default_inputs());
        let above: Vec<i64> = out
            .markers
            .iter()
            .filter(|m| m.position == MarkerPosition::AboveBar)
            .map(|m| m.time / 60_000)
            .collect();
        // Bar 2 is a strict peak; the tied 3.0 pair at bars 6 and 7 never prints.
        assert_eq!(above, vec![2]);
        assert!(
            out.markers
                .iter()
                .all(|m| if m.position == MarkerPosition::AboveBar {
                    m.shape == "triangleup" && m.color == BEARISH
                } else {
                    m.shape == "triangledown" && m.color == BULLISH
                })
        );
        assert!(
            out.markers
                .iter()
                .any(|m| m.position == MarkerPosition::BelowBar && m.time / 60_000 == 4)
        );
    }
}
