//! Input-schema builders and small color utilities shared by the catalog families.

use crate::i18n::{MessageLike, msg};
use crate::model::{FillGradientStop, InputType, LineStyle, Ohlcv, SeriesDisplay};
use crate::palette::{BEARISH, BULLISH, INFO, NEUTRAL};

use super::define::{ClassicBand, ClassicInput, ClassicLevel, ClassicPlot};
use super::math::{SOURCES, js_round};

/// The two settings sections every classic splits its inputs into.
pub const SETTINGS: &str = "Settings";
pub const STYLE: &str = "Style";

/// Default upper bound of a length / integer input.
pub const MAX_LENGTH: f64 = 5000.0;

#[allow(clippy::too_many_arguments)]
fn numeric(
    key: &str,
    title: MessageLike,
    input_type: InputType,
    defval: f64,
    min: f64,
    max: f64,
    step: f64,
    group: &str,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    let mut input = ClassicInput::new(key, title, input_type, defval).group(group);
    input.min = Some(min);
    input.max = Some(max);
    input.step = Some(step);
    input.tooltip = tooltip;
    input
}

/// A `length` input titled "Length" (1..=5000).
pub fn length_input_default(defval: f64) -> ClassicInput {
    length_input(
        defval,
        "length",
        msg("indicator.common.length", "Length"),
        MAX_LENGTH,
        None,
    )
}

/// An integer window length (min 1) in the Settings section.
pub fn length_input(
    defval: f64,
    key: &str,
    title: MessageLike,
    max: f64,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    numeric(
        key,
        title,
        InputType::Int,
        defval,
        1.0,
        max,
        1.0,
        SETTINGS,
        tooltip,
    )
}

/// An integer input in the Settings section (the original's defaults: min 1, max 5000).
pub fn int_input(
    key: &str,
    title: MessageLike,
    defval: f64,
    min: f64,
    max: f64,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    numeric(
        key,
        title,
        InputType::Int,
        defval,
        min,
        max,
        1.0,
        SETTINGS,
        tooltip,
    )
}

/// A float input in the Settings section (the original's defaults: min 0, max 1000, step 0.1).
pub fn float_input(
    key: &str,
    title: MessageLike,
    defval: f64,
    min: f64,
    max: f64,
    step: f64,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    numeric(
        key,
        title,
        InputType::Float,
        defval,
        min,
        max,
        step,
        SETTINGS,
        tooltip,
    )
}

/// A toggle (the original's default group is [`SETTINGS`]).
pub fn bool_input(
    key: &str,
    title: MessageLike,
    defval: bool,
    group: &str,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    let mut input = ClassicInput::new(key, title, InputType::Bool, defval).group(group);
    input.tooltip = tooltip;
    input
}

/// A dropdown over `options` in the Settings section.
pub fn option_input(
    key: &str,
    title: MessageLike,
    defval: &str,
    options: &[&str],
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    let mut input = ClassicInput::new(key, title, InputType::String, defval).group(SETTINGS);
    input.options = Some(options.iter().map(|o| o.to_string()).collect());
    input.tooltip = tooltip;
    input
}

/// A `source` input titled "Source", defaulting to Close.
pub fn source_input_default() -> ClassicInput {
    source_input(
        "Close",
        "source",
        msg("indicator.common.source", "Source"),
        None,
    )
}

/// A price-source dropdown over the shared [`SOURCES`] vocabulary.
pub fn source_input(
    defval: &str,
    key: &str,
    title: MessageLike,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    option_input(key, title, defval, &SOURCES, tooltip)
}

/// A `color` input titled "Color", defaulting to the informational blue.
pub fn color_input_default() -> ClassicInput {
    color_input(INFO, msg("indicator.common.color", "Color"), "color", None)
}

/// A color input in the Style section. Note the argument order: default, title, key.
pub fn color_input(
    defval: &str,
    title: MessageLike,
    key: &str,
    tooltip: Option<MessageLike>,
) -> ClassicInput {
    let mut input = ClassicInput::new(key, title, InputType::Color, defval).group(STYLE);
    input.tooltip = tooltip;
    input
}

/// A `lineWidth` input titled "Line Width" (the original's default is 2).
pub fn width_input_default(defval: f64) -> ClassicInput {
    width_input(
        defval,
        "lineWidth",
        msg("indicator.common.lineWidth", "Line Width"),
    )
}

/// A 1..=5 px line-width input in the Style section.
pub fn width_input(defval: f64, key: &str, title: MessageLike) -> ClassicInput {
    numeric(
        key,
        title,
        InputType::Int,
        defval,
        1.0,
        5.0,
        1.0,
        STYLE,
        Some(msg(
            "indicator.common.lineWidth.tooltip",
            "Width of the plotted line, in pixels.",
        )),
    )
}

fn is_hex6(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Transparency in the reference catalog's scale (0 = opaque, 100 =
/// invisible), applied over a 6-digit hex.
pub fn transp(hex: &str, transparency: f64) -> String {
    with_alpha(hex, (100.0 - transparency) / 100.0)
}

/// Default alpha of a soft band tint (see [`with_alpha`]).
pub const BAND_ALPHA: f64 = 0.08;

/// Tint derived from a line ink: alpha applied over a 6-digit hex. Anything
/// that is not `#rrggbb` comes back unchanged.
pub fn with_alpha(hex: &str, alpha: f64) -> String {
    let trimmed = hex.trim();
    match trimmed.strip_prefix('#') {
        Some(digits) if is_hex6(digits) => {
            // A NaN alpha clamps to NaN in the original and prints as "NaN"; treat it as 0.
            let a = js_round(alpha.clamp(0.0, 1.0) * 255.0);
            let a = if a.is_nan() { 0 } else { a as u8 };
            format!("#{digits}{a:02x}")
        }
        _ => hex.to_string(),
    }
}

/// The Bullish / Bearish / Basis trio a directional channel is styled with.
/// The original's default basis title is `msg("indicator.common.basisColor", "Basis")`.
pub fn channel_style_inputs(basis_title: Option<MessageLike>) -> Vec<ClassicInput> {
    let basis_title = basis_title.unwrap_or_else(|| msg("indicator.common.basisColor", "Basis"));
    vec![
        color_input(
            BULLISH,
            msg("indicator.common.bullColor", "Bullish"),
            "bullColor",
            Some(msg(
                "indicator.channel.bullColor.tooltip",
                "Color of the lower band and of the basis while price holds above it.",
            )),
        ),
        color_input(
            BEARISH,
            msg("indicator.common.bearColor", "Bearish"),
            "bearColor",
            Some(msg(
                "indicator.channel.bearColor.tooltip",
                "Color of the upper band and of the basis while price sits below it.",
            )),
        ),
        color_input(
            NEUTRAL,
            basis_title,
            "basisColor",
            Some(msg(
                "indicator.channel.basisColor.tooltip",
                "Color of the basis line while price sits exactly on it.",
            )),
        ),
    ]
}

/// The three inks of a directional study.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectionInk {
    pub bull: String,
    pub bear: String,
    pub neutral: String,
}

/// Plots and bands a shared look contributes to an output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parts {
    pub plots: Vec<ClassicPlot>,
    pub bands: Vec<ClassicBand>,
    pub levels: Vec<ClassicLevel>,
}

/// The shared look of a directional channel: a basis that takes price's side,
/// bands tinted by the side they defend, and an interior washing bearish at
/// the top down to bullish at the bottom.
pub fn directional_channel(
    bars: &[Ohlcv],
    basis: &[f64],
    upper: &[f64],
    lower: &[f64],
    ink: &DirectionInk,
    key: &str,
) -> Parts {
    let basis_colors: Vec<Option<String>> = basis
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if !b.is_finite() {
                return None;
            }
            let c = bars[i].close;
            Some(if c > b {
                ink.bull.clone()
            } else if c < b {
                ink.bear.clone()
            } else {
                ink.neutral.clone()
            })
        })
        .collect();
    let wash: Vec<Option<FillGradientStop>> = upper
        .iter()
        .enumerate()
        .map(|(i, &u)| {
            let l = lower.get(i).copied().unwrap_or(f64::NAN);
            if !u.is_finite() || !l.is_finite() {
                return None;
            }
            Some(FillGradientStop {
                top_value: u,
                bottom_value: l,
                top_color: transp(&ink.bear, 90.0),
                bottom_color: transp(&ink.bull, 90.0),
            })
        })
        .collect();
    Parts {
        plots: vec![
            ClassicPlot::new(
                "basis",
                msg("indicator.channel.plot.basis", "Basis"),
                basis.to_vec(),
                ink.neutral.clone(),
            )
            .colors(basis_colors),
            ClassicPlot::new(
                "upper",
                msg("indicator.channel.plot.upper", "Upper"),
                upper.to_vec(),
                transp(&ink.bear, 30.0),
            ),
            ClassicPlot::new(
                "lower",
                msg("indicator.channel.plot.lower", "Lower"),
                lower.to_vec(),
                transp(&ink.bull, 30.0),
            ),
        ],
        bands: vec![ClassicBand::new(key, "upper", "lower", transp(NEUTRAL, 90.0)).gradient(wash)],
        levels: Vec::new(),
    }
}

/// A constant-valued series shown nowhere: the anchor a band is filled
/// against. Its title is an identity only (it never shows), so it stays plain text.
pub fn anchor_plot(key: &str, title: &'static str, length: usize, level: f64) -> ClassicPlot {
    ClassicPlot::new(key, title, vec![level; length], transp(NEUTRAL, 100.0))
        .display(SeriesDisplay::hidden())
}

/// The thresholds and scale edges of a bounded oscillator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneLevels {
    pub overbought: f64,
    pub oversold: f64,
    pub top: f64,
    pub bottom: f64,
}

/// Default transparency of the threshold zones.
pub const ZONE_TRANSPARENCY: f64 = 92.0;

/// The soft shading a bounded oscillator wears beyond its thresholds: from the
/// overbought level up to the top of the scale, and from the oversold level
/// down to the bottom. Returns the hidden anchors and the two washes.
pub fn threshold_zones(
    length: usize,
    levels: ZoneLevels,
    overbought_ink: &str,
    oversold_ink: &str,
    transparency: f64,
) -> Parts {
    Parts {
        plots: vec![
            anchor_plot("obAnchor", "Overbought Anchor", length, levels.overbought),
            anchor_plot("osAnchor", "Oversold Anchor", length, levels.oversold),
            anchor_plot("topAnchor", "Top Anchor", length, levels.top),
            anchor_plot("bottomAnchor", "Bottom Anchor", length, levels.bottom),
        ],
        bands: vec![
            ClassicBand::new(
                "obZone",
                "obAnchor",
                "topAnchor",
                transp(overbought_ink, transparency),
            ),
            ClassicBand::new(
                "osZone",
                "osAnchor",
                "bottomAnchor",
                transp(oversold_ink, transparency),
            ),
        ],
        levels: Vec::new(),
    }
}

/// The three inks of a low → mid → high ramp.
#[derive(Clone, Debug, PartialEq)]
pub struct RampInk {
    pub low: String,
    pub mid: String,
    pub high: String,
}

/// Ink along a low → mid → high ramp: `low` ink at or under `low`, `mid` ink
/// at `mid`, `high` ink at or over `high`, blended in between.
pub fn ramp_colors(
    values: &[f64],
    low: f64,
    mid: f64,
    high: f64,
    ink: &RampInk,
) -> Vec<Option<String>> {
    values
        .iter()
        .map(|&x| {
            if !x.is_finite() {
                return None;
            }
            Some(if x <= mid {
                gradient(x, low, mid, &ink.low, &ink.mid)
            } else {
                gradient(x, mid, high, &ink.mid, &ink.high)
            })
        })
        .collect()
}

/// Default transparencies of [`baseline_gradient`]: at the value, and at the baseline.
pub const VALUE_TRANSPARENCY: f64 = 50.0;
pub const BASE_TRANSPARENCY: f64 = 100.0;

/// A vertical wash between a series and a flat baseline: opaque at the value,
/// invisible where it meets the baseline. Pair with [`anchor_plot`] for the
/// baseline anchor.
pub fn baseline_gradient(
    values: &[f64],
    baseline: f64,
    ink_of: impl Fn(f64, usize) -> String,
    value_transparency: f64,
    base_transparency: f64,
) -> Vec<Option<FillGradientStop>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            if !x.is_finite() {
                return None;
            }
            let ink = ink_of(x, i);
            let above = x > baseline;
            Some(FillGradientStop {
                top_value: x.max(baseline),
                bottom_value: x.min(baseline),
                top_color: transp(
                    &ink,
                    if above {
                        value_transparency
                    } else {
                        base_transparency
                    },
                ),
                bottom_color: transp(
                    &ink,
                    if above {
                        base_transparency
                    } else {
                        value_transparency
                    },
                ),
            })
        })
        .collect()
}

/// The four inks of a momentum histogram.
#[derive(Clone, Debug, PartialEq)]
pub struct MomentumInk {
    pub grow_above: String,
    pub fall_above: String,
    pub grow_below: String,
    pub fall_below: String,
}

/// The four-tone momentum histogram: a column is bright while its side of zero
/// is growing and faded once it turns back.
pub fn momentum_column_colors(values: &[f64], ink: &MomentumInk) -> Vec<Option<String>> {
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
            let rising = x > prev;
            Some(if x >= 0.0 {
                if rising {
                    ink.grow_above.clone()
                } else {
                    ink.fall_above.clone()
                }
            } else if rising {
                ink.grow_below.clone()
            } else {
                ink.fall_below.clone()
            })
        })
        .collect()
}

/// Options of [`bounded_oscillator`].
#[derive(Clone, Debug)]
pub struct BoundedOptions {
    pub key: String,
    pub title: MessageLike,
    pub values: Vec<f64>,
    pub overbought: f64,
    pub oversold: f64,
    pub scale_top: f64,
    pub scale_bottom: f64,
    /// Ink at the oversold end, at the midpoint, and at the overbought end of the ramp.
    pub ink: RampInk,
    pub level_ink: String,
    /// `None` ⇒ 1.
    pub width: Option<f64>,
    /// `None` ⇒ [`ZONE_TRANSPARENCY`].
    pub zone_transparency: Option<f64>,
    /// Dotted guide at a fixed price (the reference's midline).
    pub midline: Option<f64>,
    /// Wash between the two thresholds (the reference's "band fill").
    pub band_fill: Option<String>,
    /// Vertical wash from the line back to the midpoint of the band.
    pub midpoint_wash: bool,
    /// Titles of the overbought and oversold levels; `None` ⇒ the common ones.
    pub level_titles: Option<(MessageLike, MessageLike)>,
}

/// The shape every bounded oscillator shares: a line that ramps from the
/// oversold ink through neutral to the overbought ink, dashed threshold
/// levels, and soft shading beyond each threshold out to the edge of the scale.
pub fn bounded_oscillator(opts: BoundedOptions) -> Parts {
    let mid = (opts.overbought + opts.oversold) / 2.0;
    let ramp = ramp_colors(&opts.values, opts.oversold, mid, opts.overbought, &opts.ink);
    let n = opts.values.len();
    let zones = threshold_zones(
        n,
        ZoneLevels {
            overbought: opts.overbought,
            oversold: opts.oversold,
            top: opts.scale_top,
            bottom: opts.scale_bottom,
        },
        &opts.ink.high,
        &opts.ink.low,
        opts.zone_transparency.unwrap_or(ZONE_TRANSPARENCY),
    );
    let mut bands = zones.bands;
    if let Some(fill) = &opts.band_fill {
        bands.push(ClassicBand::new(
            "band",
            "obAnchor",
            "osAnchor",
            fill.clone(),
        ));
    }
    let mut extra_plots = Vec::new();
    if opts.midpoint_wash {
        extra_plots.push(anchor_plot("midAnchor", "Midpoint Anchor", n, mid));
        let wash = baseline_gradient(
            &opts.values,
            mid,
            |_x, i| ramp[i].clone().unwrap_or_else(|| opts.ink.mid.clone()),
            VALUE_TRANSPARENCY,
            BASE_TRANSPARENCY,
        );
        bands.push(
            ClassicBand::new(
                "wash",
                opts.key.clone(),
                "midAnchor",
                transp(NEUTRAL, 100.0),
            )
            .gradient(wash),
        );
    }
    let mut plots = vec![
        ClassicPlot::new(
            opts.key.clone(),
            opts.title,
            opts.values,
            opts.ink.mid.clone(),
        )
        .width(opts.width.unwrap_or(1.0))
        .colors(ramp),
    ];
    plots.extend(zones.plots);
    plots.extend(extra_plots);
    let (ob_title, os_title) = opts.level_titles.unwrap_or_else(|| {
        (
            msg("indicator.common.level.overbought", "Overbought"),
            msg("indicator.common.level.oversold", "Oversold"),
        )
    });
    let mut levels = vec![
        ClassicLevel::new("overbought", opts.overbought)
            .color(opts.level_ink.clone())
            .line_style(LineStyle::Dashed)
            .title(ob_title),
    ];
    if let Some(midline) = opts.midline {
        levels.push(
            ClassicLevel::new("midline", midline)
                .color(opts.level_ink.clone())
                .line_style(LineStyle::Dotted)
                .title(msg("indicator.common.level.midline", "Midline")),
        );
    }
    levels.push(
        ClassicLevel::new("oversold", opts.oversold)
            .color(opts.level_ink.clone())
            .line_style(LineStyle::Dashed)
            .title(os_title),
    );
    Parts {
        plots,
        bands,
        levels,
    }
}

/// Options of [`centered_oscillator`].
#[derive(Clone, Debug)]
pub struct CenteredOptions {
    pub key: String,
    pub title: MessageLike,
    pub values: Vec<f64>,
    /// Ink at the value while above / below the baseline, and exactly on it.
    pub ink: DirectionInk,
    pub zero_ink: String,
    /// `None` ⇒ 0.
    pub baseline: Option<f64>,
    /// `None` ⇒ 1.
    pub width: Option<f64>,
    /// Wash the line toward the baseline (the original's default is on).
    pub fill: bool,
    /// `None` ⇒ [`VALUE_TRANSPARENCY`].
    pub value_transparency: Option<f64>,
    /// `None` ⇒ dashed.
    pub zero_line_style: Option<LineStyle>,
    /// `None` ⇒ "Zero Line".
    pub zero_title: Option<MessageLike>,
    pub extra_plots: Vec<ClassicPlot>,
    pub extra_levels: Vec<ClassicLevel>,
}

impl CenteredOptions {
    /// The required fields, with every optional one at the original's default.
    pub fn new(
        key: impl Into<String>,
        title: impl Into<MessageLike>,
        values: Vec<f64>,
        ink: DirectionInk,
        zero_ink: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            values,
            ink,
            zero_ink: zero_ink.into(),
            baseline: None,
            width: None,
            fill: true,
            value_transparency: None,
            zero_line_style: None,
            zero_title: None,
            extra_plots: Vec::new(),
            extra_levels: Vec::new(),
        }
    }
}

/// The shape every baseline-centred study shares: a line inked by its side of
/// the baseline, washed toward it with a vertical gradient that fades out
/// where the two meet.
pub fn centered_oscillator(opts: CenteredOptions) -> Parts {
    let baseline = opts.baseline.unwrap_or(0.0);
    let ink = opts.ink.clone();
    let ink_of = move |x: f64| -> String {
        if x > baseline {
            ink.bull.clone()
        } else if x < baseline {
            ink.bear.clone()
        } else {
            ink.neutral.clone()
        }
    };
    let n = opts.values.len();
    let colors: Vec<Option<String>> = opts
        .values
        .iter()
        .map(|&x| if x.is_finite() { Some(ink_of(x)) } else { None })
        .collect();
    let mut bands = Vec::new();
    let mut anchor = None;
    if opts.fill {
        anchor = Some(anchor_plot("baseline", "Baseline Anchor", n, baseline));
        let wash = baseline_gradient(
            &opts.values,
            baseline,
            |x, _i| ink_of(x),
            opts.value_transparency.unwrap_or(VALUE_TRANSPARENCY),
            BASE_TRANSPARENCY,
        );
        bands.push(
            ClassicBand::new("wash", opts.key.clone(), "baseline", transp(NEUTRAL, 100.0))
                .gradient(wash),
        );
    }
    let mut plots = vec![
        ClassicPlot::new(
            opts.key.clone(),
            opts.title,
            opts.values,
            opts.ink.neutral.clone(),
        )
        .width(opts.width.unwrap_or(1.0))
        .colors(colors),
    ];
    plots.extend(opts.extra_plots);
    plots.extend(anchor);
    let mut levels = vec![
        ClassicLevel::new("zero", baseline)
            .color(opts.zero_ink)
            .line_style(opts.zero_line_style.unwrap_or(LineStyle::Dashed))
            .title(
                opts.zero_title
                    .unwrap_or_else(|| msg("indicator.common.level.zero", "Zero Line")),
            ),
    ];
    levels.extend(opts.extra_levels);
    Parts {
        plots,
        bands,
        levels,
    }
}

fn channels(hex: &str) -> Option<[f64; 3]> {
    // Only the first six hex digits count, so an `#rrggbbaa` ink blends by its color.
    let digits = hex.trim().strip_prefix('#')?.get(..6)?;
    if !is_hex6(digits) {
        return None;
    }
    let n = u32::from_str_radix(digits, 16).ok()?;
    Some([
        f64::from((n >> 16) & 255),
        f64::from((n >> 8) & 255),
        f64::from(n & 255),
    ])
}

/// Blend `from`→`to` by where `value` falls in `[low, high]`, clamped at both
/// ends (a non-finite value or an unparseable ink yields `from`).
pub fn gradient(value: f64, low: f64, high: f64, from: &str, to: &str) -> String {
    let (Some(a), Some(b)) = (channels(from), channels(to)) else {
        return from.to_string();
    };
    if !value.is_finite() {
        return from.to_string();
    }
    let span = high - low;
    let t = if span == 0.0 {
        0.0
    } else {
        ((value - low) / span).clamp(0.0, 1.0)
    };
    if t.is_nan() {
        // A NaN bound (a range still warming up). The original prints an
        // unparseable "#NaNNaNNaN" here; hand back the starting ink instead.
        return from.to_string();
    }
    let mix = |i: usize| js_round(a[i] + (b[i] - a[i]) * t) as u8;
    format!("#{:02x}{:02x}{:02x}", mix(0), mix(1), mix(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparency_maps_to_a_hex_alpha() {
        assert_eq!(transp("#f23645", 40.0), "#f2364599");
        assert_eq!(transp("#089981", 40.0), "#08998199");
        assert_eq!(transp(NEUTRAL, 95.0), "#787b860d");
        assert_eq!(transp(NEUTRAL, 100.0), "#787b8600");
        assert_eq!(with_alpha("#ABCDEF", BAND_ALPHA), "#ABCDEF14");
    }

    #[test]
    fn a_non_hex6_ink_is_left_alone() {
        assert_eq!(with_alpha("#fff", 0.5), "#fff");
        assert_eq!(with_alpha("rgba(1,2,3,0.5)", 0.5), "rgba(1,2,3,0.5)");
        assert_eq!(with_alpha("#12345678", 0.5), "#12345678");
    }

    #[test]
    fn gradient_blends_and_clamps() {
        assert_eq!(gradient(5.0, 0.0, 10.0, "#000000", "#ffffff"), "#808080");
        assert_eq!(gradient(-5.0, 0.0, 10.0, "#000000", "#ffffff"), "#000000");
        assert_eq!(gradient(50.0, 0.0, 10.0, "#000000", "#ffffff"), "#ffffff");
        assert_eq!(
            gradient(f64::NAN, 0.0, 10.0, "#010203", "#ffffff"),
            "#010203"
        );
        assert_eq!(gradient(5.0, 0.0, 10.0, "red", "#ffffff"), "red");
        assert_eq!(gradient(1.0, 3.0, 3.0, "#102030", "#ffffff"), "#102030");
        assert_eq!(
            gradient(1.0, f64::NAN, 3.0, "#102030", "#ffffff"),
            "#102030"
        );
    }

    #[test]
    fn momentum_columns_brighten_while_growing() {
        let ink = MomentumInk {
            grow_above: "ga".into(),
            fall_above: "fa".into(),
            grow_below: "gb".into(),
            fall_below: "fb".into(),
        };
        let colors = momentum_column_colors(&[1.0, 2.0, 1.0, -1.0, -0.5, f64::NAN], &ink);
        let got: Vec<Option<&str>> = colors.iter().map(|c| c.as_deref()).collect();
        assert_eq!(
            got,
            vec![
                Some("fa"),
                Some("ga"),
                Some("fa"),
                Some("fb"),
                Some("gb"),
                None
            ]
        );
    }

    #[test]
    fn bounded_oscillator_orders_levels_with_the_midline_between() {
        let parts = bounded_oscillator(BoundedOptions {
            key: "rsi".into(),
            title: "RSI".into(),
            values: vec![f64::NAN, 80.0, 20.0],
            overbought: 70.0,
            oversold: 30.0,
            scale_top: 100.0,
            scale_bottom: 0.0,
            ink: RampInk {
                low: BULLISH.into(),
                mid: NEUTRAL.into(),
                high: BEARISH.into(),
            },
            level_ink: NEUTRAL.into(),
            width: None,
            zone_transparency: None,
            midline: Some(50.0),
            band_fill: None,
            midpoint_wash: true,
            level_titles: None,
        });
        let level_keys: Vec<&str> = parts.levels.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(level_keys, vec!["overbought", "midline", "oversold"]);
        let plot_keys: Vec<&str> = parts.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            plot_keys,
            vec![
                "rsi",
                "obAnchor",
                "osAnchor",
                "topAnchor",
                "bottomAnchor",
                "midAnchor"
            ]
        );
        let colors = parts.plots[0].colors.as_ref().unwrap();
        assert_eq!(colors[0], None);
        assert_eq!(colors[1].as_deref(), Some(BEARISH));
        assert_eq!(colors[2].as_deref(), Some(BULLISH));
        let band_keys: Vec<&str> = parts.bands.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(band_keys, vec!["obZone", "osZone", "wash"]);
    }

    #[test]
    fn centered_oscillator_keeps_extra_plots_before_the_anchor() {
        let ink = DirectionInk {
            bull: BULLISH.into(),
            bear: BEARISH.into(),
            neutral: NEUTRAL.into(),
        };
        let mut opts = CenteredOptions::new("osc", "Osc", vec![1.0, -1.0, 0.0], ink, NEUTRAL);
        opts.extra_plots = vec![ClassicPlot::new("signal", "Signal", vec![0.0; 3], INFO)];
        let parts = centered_oscillator(opts);
        let plot_keys: Vec<&str> = parts.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(plot_keys, vec!["osc", "signal", "baseline"]);
        let colors = parts.plots[0].colors.as_ref().unwrap();
        assert_eq!(colors[0].as_deref(), Some(BULLISH));
        assert_eq!(colors[1].as_deref(), Some(BEARISH));
        assert_eq!(colors[2].as_deref(), Some(NEUTRAL));
        assert_eq!(parts.levels[0].price, 0.0);
    }
}
