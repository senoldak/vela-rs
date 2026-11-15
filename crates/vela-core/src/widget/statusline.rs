//! In-chart status line (top-left overlay): symbol + the OHLC/change of the bar under the
//! crosshair, falling back to the latest live bar.
//!
//! The original `Statusline` is a DOM component. This module is its pure half:
//!
//! - the market session vocabulary ([`MarketStatus`]: labels, inks, badge icon ids);
//! - [`statusline_ink_of`]: the active price style's value colors, direction rule and
//!   readout shape, read from the renderer;
//! - [`statusline_values`]: the value readout text for one bar at one ladder level
//!   (`renderValues` without the elements);
//! - [`width_key`]: everything the readout's width depends on, so a caller can skip the
//!   ladder walk when only digits changed;
//! - the symbol and meta text helpers.
//!
//! Left for the browser crate: the element tree and its CSS, `ResizeObserver` fitting
//! (walking [`STATUSLINE_LADDER`](super::statusline_model::STATUSLINE_LADDER) against the
//! measured overflow and hiding segments in fit mode), tooltips, the context menu widget
//! (its rows are [`statusline_menu_items`](super::statusline_model::statusline_menu_items)),
//! chart event subscriptions (`bar`, `replay:*`, crosshair) and the replay badge drawing.

use std::rc::Rc;

use serde_json::Value;

use super::format::decimals_for;
use super::statusline_model::{ReadoutCellKey, ReadoutShape, StatuslineLevel, readout_cells};
use crate::format::types::{FormatSurface, PercentFormatOptions, PriceFormatOptions};
use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use crate::palette::{SESSION_OFF, SESSION_POST, SESSION_PRE};
use crate::render::core::color::with_alpha;
use crate::util::to_fixed;

/// How the status line reads a bar out (an alias of the model's shape): the four O/H/L/C
/// values (bar-shaped styles), or the single plotted value, the close, for the single-value
/// styles (the line family, area, baseline, columns).
pub type StatuslineReadout = ReadoutShape;

/// The market session states the status badge can wear. Crypto venues trade
/// continuously and stay `Open`; the full vocabulary is ready for providers that
/// carry a session model (equities RTH/ETH, exchange holidays). Overnight roll
/// tapes wear the single `Extended` state: they have no pre/post split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarketStatus {
    Open,
    Pre,
    Post,
    Extended,
    Closed,
    Holiday,
}

impl MarketStatus {
    pub const ALL: [MarketStatus; 6] = [
        MarketStatus::Open,
        MarketStatus::Pre,
        MarketStatus::Post,
        MarketStatus::Extended,
        MarketStatus::Closed,
        MarketStatus::Holiday,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MarketStatus::Open => "open",
            MarketStatus::Pre => "pre",
            MarketStatus::Post => "post",
            MarketStatus::Extended => "extended",
            MarketStatus::Closed => "closed",
            MarketStatus::Holiday => "holiday",
        }
    }

    /// The hover label.
    pub fn label(self) -> MessageLike {
        match self {
            MarketStatus::Open => msg("widget.statusline.market.open", "Market Open"),
            MarketStatus::Pre => msg("widget.statusline.market.pre", "Pre-Market"),
            MarketStatus::Post => msg("widget.statusline.market.post", "Post-Market"),
            MarketStatus::Extended => msg("widget.statusline.market.extended", "Extended Hours"),
            MarketStatus::Closed => msg("widget.statusline.market.closed", "Market Closed"),
            MarketStatus::Holiday => msg("widget.statusline.market.holiday", "Market Holiday"),
        }
    }

    /// Session ink: open wears the theme's up color; the other sessions are meaning
    /// constants from the palette (pre, post and extended, gray closed/holiday). The badge
    /// circle is the same ink at a 20% wash.
    pub fn ink(self) -> &'static str {
        match self {
            MarketStatus::Open => "var(--vela-up)",
            MarketStatus::Pre => SESSION_PRE,
            MarketStatus::Post | MarketStatus::Extended => SESSION_POST,
            MarketStatus::Closed | MarketStatus::Holiday => SESSION_OFF,
        }
    }

    /// The badge icon id (`market-open`, ...).
    pub fn icon(self) -> String {
        format!("market-{}", self.as_str())
    }

    /// The badge circle's background: the ink at a 20% wash.
    pub fn badge_background(self) -> String {
        format!("color-mix(in srgb, {} 20%, transparent)", self.ink())
    }
}

/// The badge label while the chart replays past bars.
pub fn replay_label() -> MessageLike {
    msg("widget.statusline.replay", "Replay Mode")
}

const EM_DASH: &str = "\u{2014}";

/// The O/H/L/C values of the bar a readout shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarLike {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// The ticker without its quote suffix (`BTCUSDT` becomes `BTC`); the ticker itself when
/// stripping would leave nothing.
pub fn base_of_ticker(ticker: &str) -> String {
    const SUFFIXES: [&str; 8] = ["USDT", "USDC", "USD1", "USDS", "BUSD", "USD", "EUR", "PERP"];
    let lower = ticker.to_ascii_lowercase(); // same byte length as the ticker
    for suffix in SUFFIXES {
        if lower.ends_with(&suffix.to_ascii_lowercase()) {
            let mut base = &ticker[..ticker.len() - suffix.len()];
            if let Some(stripped) = base
                .strip_suffix('-')
                .or_else(|| base.strip_suffix('_'))
                .or_else(|| base.strip_suffix('/'))
            {
                base = stripped;
            }
            return if base.is_empty() {
                ticker.to_string()
            } else {
                base.to_string()
            };
        }
    }
    ticker.to_string()
}

/// The renderer reads the ink derivation needs: `chart.renderer` satisfies it.
pub trait RendererReads {
    fn get_config(&self) -> Option<Value>;
    fn get(&self, key: &str) -> Option<Value>;
}

/// The direction rule: whether a bar wears the up color.
pub type IsUp = Rc<dyn Fn(f64, f64) -> bool>;

/// What [`statusline_ink_of`] derives.
#[derive(Clone)]
pub struct StatuslineInk {
    pub up: Option<String>,
    pub down: Option<String>,
    /// Replaces the close-vs-open rule where the style paints by something else
    /// (arguments: open, close).
    pub is_up: Option<IsUp>,
    pub readout: StatuslineReadout,
}

/// The active price style's value readout for the status line, read live from the
/// renderer: the up/down COLORS from the cosmetic config (candle bodies for candles/HA,
/// and plugin styles, which paint candles as their base; bar ticks for bars; the single
/// plot color for the line family and area; the two baseline line colors for baseline;
/// the column colors made opaque for columns), the DIRECTION rule that decides which of
/// the two a bar wears, and the readout SHAPE. Candles/bars compare close to open and show
/// O/H/L/C; single-value styles show just the plotted value; baseline compares close to
/// the LIVE baseline reference price (the paint splits by position, not by bar direction:
/// a bar that closed down can still sit in the green region), and columns compare it to
/// the previous bar's close, as they are painted. Missing colors fall back to the theme
/// tokens.
pub fn statusline_ink_of(renderer: Rc<dyn RendererReads>, price_style: &str) -> StatuslineInk {
    let cfg = renderer.get_config();
    let color = |section: &str, key: &str| -> Option<String> {
        cfg.as_ref()?
            .get(section)?
            .get(key)?
            .as_str()
            .map(str::to_string)
    };
    let ink = |up, down, is_up, readout| StatuslineInk {
        up,
        down,
        is_up,
        readout,
    };
    match price_style {
        "bars" => ink(
            color("bars", "upColor"),
            color("bars", "downColor"),
            None,
            ReadoutShape::Ohlc,
        ),
        "line" | "linemarkers" | "stepline" => {
            let v = color("line", "color");
            ink(v.clone(), v, None, ReadoutShape::Value)
        }
        "area" => {
            let v = color("area", "lineColor");
            ink(v.clone(), v, None, ReadoutShape::Value)
        }
        "baseline" => {
            // Resolved per render: the percent-level baseline moves with the visible range.
            let reads = Rc::clone(&renderer);
            let is_up: IsUp = Rc::new(move |open, close| {
                let level = number_of(reads.get("baselinePrice"));
                if level.is_finite() {
                    close >= level
                } else {
                    close >= open
                }
            });
            ink(
                color("baseline", "topLineColor"),
                color("baseline", "bottomLineColor"),
                Some(is_up),
                ReadoutShape::Value,
            )
        }
        "columns" => {
            // Resolved per render: the previous close follows the bar the readout shows. The
            // first bar has none and is up, as its column is.
            let reads = Rc::clone(&renderer);
            let is_up: IsUp = Rc::new(move |_open, close| match reads.get("previousClose") {
                Some(Value::Number(n)) => match n.as_f64() {
                    Some(prev) if prev.is_finite() => close >= prev,
                    _ => true,
                },
                _ => true,
            });
            // Text in the columns' translucent wash would read faint: the readout wears it opaque.
            let solid = |key: &str| color("columns", key).map(|v| with_alpha(&v, 1.0));
            ink(
                solid("upColor"),
                solid("downColor"),
                Some(is_up),
                ReadoutShape::Value,
            )
        }
        _ => ink(
            color("candles", "upColor"),
            color("candles", "downColor"),
            None,
            ReadoutShape::Ohlc,
        ),
    }
}

/// JavaScript's `Number(value)` for the JSON values a renderer feature returns.
fn number_of(value: Option<Value>) -> f64 {
    match value {
        None | Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(b)),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// The "· BINANCE · 1h" segment after the symbol: venue first, then resolution.
pub fn meta_text(timeframe: &str, provider: &str, i18n: Option<&dyn Translator>) -> String {
    let venue = if provider.is_empty() {
        String::new()
    } else {
        format!("\u{b7} {} ", provider.to_uppercase())
    };
    format!(
        "{venue}\u{b7} {}",
        super::timeframe::timeframe_label(timeframe, i18n)
    )
}

/// Everything the readout's width depends on, at every rung: its shape, and each
/// value's sign, digit count and decimals (the digits themselves are tabular, so any
/// digit reads as `0`; the grouping commas follow from the digit count). Cheap enough to
/// run per pointer move. The parts, the host's size and the fonts re-fit through their
/// own paths.
pub fn width_key(bar: Option<&BarLike>, readout: StatuslineReadout) -> String {
    let shape = match readout {
        ReadoutShape::Ohlc => "ohlc",
        ReadoutShape::Value => "value",
    };
    let Some(bar) = bar else {
        return shape.to_string();
    };
    let dp = decimals_for(bar.close);
    let diff = bar.close - bar.open;
    let mut text: Vec<String> = [bar.open, bar.high, bar.low, bar.close, diff]
        .iter()
        .map(|v| to_fixed(*v, dp))
        .collect();
    text.push(to_fixed((diff / bar.open) * 100.0, 2));
    format!("{shape} {}", text.join(" "))
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect()
}

/// One value cell of the readout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueCell {
    pub label: String,
    pub text: String,
}

/// The value readout of one bar at one ladder level.
#[derive(Clone, Debug, PartialEq)]
pub struct StatuslineValues {
    pub cells: Vec<ValueCell>,
    /// The bar change beside the values; empty when the open or close is unusable.
    pub change: String,
    /// Whether the bar wears the up ink (the change's `data-dir`).
    pub up: bool,
}

/// Write the value readout for `bar` at the current ladder `level`: bar-shaped styles read
/// out all four values at the full level and just the close below it; a one-line style
/// plots a single series, so its readout is that value (the close) at every level. The
/// minimal level keeps the percent delta alone. `is_up` is the style's direction rule
/// (`None`: close at or above open).
pub fn statusline_values(
    bar: &BarLike,
    level: StatuslineLevel,
    readout: StatuslineReadout,
    is_up: Option<&IsUp>,
    i18n: Option<&dyn Translator>,
) -> StatuslineValues {
    let dp = decimals_for(bar.close);
    let up = match is_up {
        Some(rule) => rule(bar.open, bar.close),
        None => bar.close >= bar.open,
    };
    // Grouped like a price readout (`1,234.50`); English when no chart is bound yet.
    let price = |v: f64| -> String {
        if !v.is_finite() {
            return EM_DASH.to_string();
        }
        match i18n {
            Some(t) => t.format().price(
                v,
                &PriceFormatOptions {
                    surface: Some(FormatSurface::StatusLine),
                    decimals: dp as f64,
                    mintick: None,
                    mode: None,
                    pane_id: None,
                    grouping: Some(true),
                },
            ),
            None => super::format::fmt_price(Some(v), Some(dp)),
        }
    };
    let percent = |v: f64| -> String {
        match i18n {
            Some(t) => t.format().percent(
                v,
                &PercentFormatOptions {
                    surface: Some(FormatSurface::StatusLine),
                    ..PercentFormatOptions::default()
                },
            ),
            None => format!(
                "{}{}%",
                if v >= 0.0 { "+" } else { "-" },
                to_fixed(v.abs(), 2)
            ),
        }
    };
    let cells = readout_cells(level, readout, i18n)
        .into_iter()
        .map(|c| {
            let v = match c.key {
                ReadoutCellKey::Open => bar.open,
                ReadoutCellKey::High => bar.high,
                ReadoutCellKey::Low => bar.low,
                ReadoutCellKey::Close => bar.close,
            };
            ValueCell {
                label: c.label,
                text: price(v),
            }
        })
        .collect();
    let mut change = String::new();
    if bar.open.is_finite() && bar.close.is_finite() && bar.open != 0.0 {
        let diff = bar.close - bar.open;
        let pct = percent((diff / bar.open) * 100.0);
        let signed = format!("{}{}", if diff >= 0.0 { "+" } else { "" }, price(diff));
        change = if level == StatuslineLevel::Minimal {
            pct
        } else {
            text_of(
                i18n,
                &msg("widget.statusline.change", "{change} ({percent})"),
                &[
                    ("change", MessageArg::from(signed)),
                    ("percent", MessageArg::from(pct)),
                ],
            )
        };
    }
    StatuslineValues { cells, change, up }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fake {
        config: Value,
        feats: Vec<(&'static str, Value)>,
    }

    impl RendererReads for Fake {
        fn get_config(&self) -> Option<Value> {
            Some(self.config.clone())
        }
        fn get(&self, key: &str) -> Option<Value> {
            self.feats
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }
    }

    fn fake(config: Value, feats: Vec<(&'static str, Value)>) -> Rc<dyn RendererReads> {
        Rc::new(Fake { config, feats })
    }

    #[test]
    fn candles_and_unknown_styles_read_the_candle_colors_as_ohlc() {
        let r = fake(
            json!({ "candles": { "upColor": "#0f0", "downColor": "#f00" } }),
            vec![],
        );
        for style in ["candles", "heikinashi", "some-plugin-style"] {
            let ink = statusline_ink_of(Rc::clone(&r), style);
            assert_eq!(ink.up.as_deref(), Some("#0f0"));
            assert_eq!(ink.down.as_deref(), Some("#f00"));
            assert!(ink.is_up.is_none());
            assert_eq!(ink.readout, ReadoutShape::Ohlc);
        }
    }

    #[test]
    fn bars_read_the_tick_colors() {
        let r = fake(
            json!({ "bars": { "upColor": "#111", "downColor": "#222" } }),
            vec![],
        );
        let ink = statusline_ink_of(r, "bars");
        assert_eq!(
            (ink.up.as_deref(), ink.down.as_deref()),
            (Some("#111"), Some("#222"))
        );
        assert_eq!(ink.readout, ReadoutShape::Ohlc);
    }

    #[test]
    fn the_line_family_and_area_show_one_value_in_the_plot_color() {
        let r = fake(
            json!({ "line": { "color": "#abc" }, "area": { "lineColor": "#def" } }),
            vec![],
        );
        for style in ["line", "linemarkers", "stepline"] {
            let ink = statusline_ink_of(Rc::clone(&r), style);
            assert_eq!(
                (ink.up.as_deref(), ink.down.as_deref()),
                (Some("#abc"), Some("#abc"))
            );
            assert_eq!(ink.readout, ReadoutShape::Value);
        }
        assert_eq!(statusline_ink_of(r, "area").up.as_deref(), Some("#def"));
    }

    #[test]
    fn baseline_compares_the_close_to_the_live_baseline_price() {
        let r = fake(
            json!({ "baseline": { "topLineColor": "#0a0", "bottomLineColor": "#a00" } }),
            vec![("baselinePrice", json!(100.0))],
        );
        let ink = statusline_ink_of(r, "baseline");
        let is_up = ink.is_up.expect("a direction rule");
        assert!(is_up(120.0, 101.0)); // closed down but still above the baseline
        assert!(!is_up(90.0, 99.0));
        // No usable baseline price: the close-vs-open rule.
        let none = statusline_ink_of(
            fake(json!({}), vec![("baselinePrice", json!("junk"))]),
            "baseline",
        );
        let rule = none.is_up.expect("a direction rule");
        assert!(rule(1.0, 2.0));
        assert!(!rule(2.0, 1.0));
    }

    #[test]
    fn columns_compare_the_close_to_the_previous_close_and_wear_opaque_colors() {
        let r = fake(
            json!({ "columns": { "upColor": "rgba(0,255,0,0.4)", "downColor": "#ff0000" } }),
            vec![("previousClose", json!(50.0))],
        );
        let ink = statusline_ink_of(r, "columns");
        assert_eq!(
            ink.up.as_deref(),
            Some(with_alpha("rgba(0,255,0,0.4)", 1.0).as_str())
        );
        let rule = ink.is_up.expect("a direction rule");
        assert!(rule(0.0, 50.0));
        assert!(!rule(0.0, 49.0));
        // The first bar has no previous close and is up.
        let first = statusline_ink_of(fake(json!({}), vec![]), "columns");
        assert!(first.is_up.expect("a direction rule")(10.0, 1.0));
    }

    #[test]
    fn missing_colors_fall_back_to_none_so_the_theme_tokens_apply() {
        let ink = statusline_ink_of(fake(json!(null), vec![]), "candles");
        assert!(ink.up.is_none() && ink.down.is_none());
        let ink = statusline_ink_of(
            fake(json!({ "candles": { "upColor": 3 } }), vec![]),
            "candles",
        );
        assert!(ink.up.is_none());
    }

    #[test]
    fn the_market_vocabulary_is_complete() {
        let names: Vec<&str> = MarketStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            ["open", "pre", "post", "extended", "closed", "holiday"]
        );
        assert_eq!(text_of(None, &MarketStatus::Pre.label(), &[]), "Pre-Market");
        assert_eq!(
            text_of(None, &MarketStatus::Open.label(), &[]),
            "Market Open"
        );
        assert_eq!(MarketStatus::Open.ink(), "var(--vela-up)");
        assert_eq!(MarketStatus::Extended.ink(), MarketStatus::Post.ink());
        assert_eq!(MarketStatus::Holiday.icon(), "market-holiday");
        assert_eq!(
            MarketStatus::Closed.badge_background(),
            format!("color-mix(in srgb, {SESSION_OFF} 20%, transparent)")
        );
        assert_eq!(text_of(None, &replay_label(), &[]), "Replay Mode");
    }

    #[test]
    fn base_of_ticker_strips_the_quote_suffix() {
        assert_eq!(base_of_ticker("BTCUSDT"), "BTC");
        assert_eq!(base_of_ticker("ETH-USD"), "ETH");
        assert_eq!(base_of_ticker("SOL_PERP"), "SOL");
        assert_eq!(base_of_ticker("eth/eur"), "eth");
        assert_eq!(base_of_ticker("USDT"), "USDT");
        assert_eq!(base_of_ticker("AAPL"), "AAPL");
    }

    #[test]
    fn meta_reads_venue_then_resolution() {
        assert_eq!(meta_text("60", "binance", None), "\u{b7} BINANCE \u{b7} 1h");
        assert_eq!(meta_text("D", "", None), "\u{b7} 1D");
    }

    #[test]
    fn the_width_key_ignores_digits_but_not_shape_sign_or_length() {
        let a = BarLike {
            open: 100.0,
            high: 110.0,
            low: 90.0,
            close: 105.0,
        };
        let b = BarLike {
            open: 123.4,
            high: 198.7,
            low: 54.3,
            close: 128.9,
        };
        assert_eq!(
            width_key(Some(&a), ReadoutShape::Ohlc),
            width_key(Some(&b), ReadoutShape::Ohlc)
        );
        let big = BarLike {
            open: 1000.0,
            high: 1100.0,
            low: 900.0,
            close: 1050.0,
        };
        assert_ne!(
            width_key(Some(&a), ReadoutShape::Ohlc),
            width_key(Some(&big), ReadoutShape::Ohlc)
        );
        let down = BarLike { close: 95.0, ..a };
        assert_ne!(
            width_key(Some(&a), ReadoutShape::Ohlc),
            width_key(Some(&down), ReadoutShape::Ohlc)
        );
        assert_eq!(width_key(None, ReadoutShape::Value), "value");
        assert_ne!(
            width_key(Some(&a), ReadoutShape::Ohlc),
            width_key(Some(&a), ReadoutShape::Value)
        );
    }

    fn cells(v: &StatuslineValues) -> Vec<(&str, &str)> {
        v.cells
            .iter()
            .map(|c| (c.label.as_str(), c.text.as_str()))
            .collect()
    }

    #[test]
    fn the_full_readout_lists_all_four_values_and_the_signed_change() {
        let bar = BarLike {
            open: 100.0,
            high: 110.0,
            low: 95.5,
            close: 105.0,
        };
        let v = statusline_values(&bar, StatuslineLevel::Full, ReadoutShape::Ohlc, None, None);
        assert_eq!(
            cells(&v),
            [
                ("O", "100.00"),
                ("H", "110.00"),
                ("L", "95.50"),
                ("C", "105.00")
            ]
        );
        assert_eq!(v.change, "+5.00 (+5.00%)");
        assert!(v.up);
    }

    #[test]
    fn a_down_bar_has_no_plus_and_wears_the_down_ink() {
        let bar = BarLike {
            open: 100.0,
            high: 101.0,
            low: 90.0,
            close: 95.0,
        };
        let v = statusline_values(&bar, StatuslineLevel::Full, ReadoutShape::Ohlc, None, None);
        assert_eq!(v.change, "-5.00 (-5.00%)");
        assert!(!v.up);
    }

    #[test]
    fn lower_levels_shed_values_and_the_minimal_one_keeps_the_percent_alone() {
        let bar = BarLike {
            open: 100.0,
            high: 110.0,
            low: 95.5,
            close: 105.0,
        };
        let compact = statusline_values(
            &bar,
            StatuslineLevel::Compact,
            ReadoutShape::Ohlc,
            None,
            None,
        );
        assert_eq!(cells(&compact), [("C", "105.00")]);
        assert_eq!(compact.change, "+5.00 (+5.00%)");
        let minimal = statusline_values(
            &bar,
            StatuslineLevel::Minimal,
            ReadoutShape::Ohlc,
            None,
            None,
        );
        assert_eq!(cells(&minimal), [("", "105.00")]);
        assert_eq!(minimal.change, "+5.00%");
        let line = statusline_values(&bar, StatuslineLevel::Full, ReadoutShape::Value, None, None);
        assert_eq!(cells(&line), [("", "105.00")]);
    }

    #[test]
    fn unusable_values_read_as_a_dash_and_an_unusable_open_has_no_change() {
        let bar = BarLike {
            open: 0.0,
            high: f64::NAN,
            low: 1.0,
            close: 5.0,
        };
        let v = statusline_values(&bar, StatuslineLevel::Full, ReadoutShape::Ohlc, None, None);
        assert_eq!(v.cells[1].text, "\u{2014}");
        assert_eq!(v.change, "");
    }

    #[test]
    fn a_direction_rule_overrides_close_versus_open() {
        let bar = BarLike {
            open: 100.0,
            high: 101.0,
            low: 90.0,
            close: 95.0,
        };
        let always_up: IsUp = Rc::new(|_, _| true);
        let v = statusline_values(
            &bar,
            StatuslineLevel::Full,
            ReadoutShape::Ohlc,
            Some(&always_up),
            None,
        );
        assert!(v.up);
    }
}
