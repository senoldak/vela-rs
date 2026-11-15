//! Dumps the answers of the small pure core functions (timeframes, Heikin
//! Ashi, extended tickers, visible-range presets, symbol helpers, the state
//! document sanitizer) as JSON, inputs included. The output is the Rust half
//! of a parity check against the original TypeScript (see the README).
//!
//! Usage: `cargo run --release --example dump_misc > misc.json`

use serde_json::{Value, json};
use vela_core::chart_types::ChartTypeRegistry;
use vela_core::data::{
    base_of, default_member_of, group_key_of, group_members, is_group_row, ledger_crypto_icon_url,
    timeframe_to_ms,
};
use vela_core::model::Ohlcv;
use vela_core::ports::data_provider::SymbolDescriptor;
use vela_core::price_styles::{heikin_ashi_full, heikin_ashi_next, parse_extended_ticker};
use vela_core::state::{encode_state, sanitize_state};
use vela_core::visible_range::{VisibleRangePreset, preset_to_range};

/// Lehmer generator: every product stays under 2^53, so the numbers are exact in JavaScript too.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = (self.0 * 48271) % 2_147_483_647;
        self.0
    }
}

fn walk(count: i64, start: i64, step_ms: i64) -> Vec<Ohlcv> {
    let mut rng = Rng(31_337);
    let mut close_cents: i64 = 10_000;
    (0..count)
        .map(|i| {
            let open_cents = close_cents;
            close_cents = (close_cents + (rng.next() % 201) as i64 - 100).max(1_000);
            let high_cents = open_cents.max(close_cents) + (rng.next() % 80) as i64;
            let low_cents = open_cents.min(close_cents) - (rng.next() % 80) as i64;
            Ohlcv {
                time: start + i * step_ms,
                open: open_cents as f64 / 100.0,
                high: high_cents as f64 / 100.0,
                low: low_cents as f64 / 100.0,
                close: close_cents as f64 / 100.0,
                volume: (i % 7 != 0).then(|| (1_000 + rng.next() % 5_000) as f64),
            }
        })
        .collect()
}

fn bar(b: &Ohlcv) -> Value {
    json!({ "time": b.time, "open": b.open, "high": b.high, "low": b.low, "close": b.close, "volume": b.volume })
}

fn bars(list: &[Ohlcv]) -> Value {
    Value::Array(list.iter().map(bar).collect())
}

/// State documents: one clean, the rest malformed in the ways untrusted storage goes wrong.
fn state_docs() -> Vec<Value> {
    let chart = json!({
        "id": "c1",
        "symbol": "BTCUSDT",
        "provider": "binance",
        "timeframe": "60",
        "priceStyle": "heikinashi",
        "bars": 500,
        "session": "extended",
        "watermark": false,
        "replayWatermark": true,
        "indicatorTitles": true,
        "indicatorValues": false,
        "statuslineParts": { "logo": true, "ohlc": false, "bogus": true, "name": "yes" },
        "rendererConfig": { "theme": "dark", "nested": [1, 2, { "a": null }] },
        "drawings": { "version": 1, "drawings": [] },
        "indicators": {
            "manifest": ["RSI", { "name": "MACD", "inputs": { "fast": 12 }, "props": { "precision": 2 }, "hidden": true }, { "inputs": {} }, 5],
            "natives": ["volume", { "type": "sma", "inputs": { "length": 50 }, "hidden": false }, { "type": "linear-regression" }, { "type": "linear-regression", "inputs": { "mode": "Slope" } }, null]
        },
        "ext": { "plugin": { "k": 1 }, "empty": null }
    });
    vec![
        json!({
            "version": 1,
            "layout": "2h",
            "trackSizes": { "2h": { "cols": [0.6, 0.4], "rows": [1] }, "bad": { "cols": "x" } },
            "activeCellId": "c1",
            "sync": { "viewport": true, "symbol": false, "crosshair": { "c1": "a", "c2": "a" }, "style": "nope", "unknown": true },
            "timezone": "Europe/Istanbul",
            "favorites": ["trendline", "hline", 5, "trendline"],
            "timeframeFavorites": ["60", "D"],
            "drawingTools": { "defaults": { "trendline": { "style": { "lineColor": "#fff" } } }, "templates": { "box": [{ "name": "A", "settings": {} }] }, "magnet": "weak", "stay": true },
            "panels": { "open": "watchlist", "widths": { "watchlist": 320, "bad": "wide" }, "pinned": ["watchlist", 3] },
            "charts": [chart.clone(), { "id": "c2" }, { "symbol": "no id" }, "garbage", { "id": 7 }],
            "ext": { "host": [1, 2, 3] }
        }),
        json!({ "version": 1, "layout": "single", "charts": [{ "id": "only" }] }),
        json!({ "version": 1, "layout": "single", "charts": [] }),
        json!({ "version": 1, "layout": "single" }),
        json!({ "version": 2, "layout": "single", "charts": [{ "id": "a" }] }),
        json!({ "version": "1", "layout": "single", "charts": [{ "id": "a" }] }),
        json!({ "version": 1, "layout": 5, "charts": [{ "id": "a" }] }),
        json!({ "version": 1, "layout": "", "charts": [{ "id": "a" }] }),
        json!({ "version": 1, "layout": "4", "charts": [{ "id": "a", "bars": -5, "timeframe": 60, "symbol": "", "watermark": "yes" }, { "id": "a" }, { "id": "" }] }),
        json!({ "version": 1, "layout": "single", "charts": { "0": { "id": "obj" } } }),
        json!({ "version": 1, "layout": "single", "charts": [{ "id": "a", "bars": 1.5 }, { "id": "b", "bars": 0 }, { "id": "c", "bars": 1e9 }], "drawingTools": { "magnet": "sticky", "stay": "no" }, "panels": [], "sync": [], "trackSizes": [] }),
        json!({ "version": 1, "layout": "single", "charts": [{ "id": "a", "indicators": { "manifest": "x", "natives": {} } }, { "id": "b", "indicators": [] }, { "id": "c", "statuslineParts": [], "ext": [] }] }),
        json!(null),
        json!([]),
        json!("text"),
        json!(42),
    ]
}

fn symbol_pool() -> Vec<Value> {
    vec![
        json!({ "ticker": "BTCUSDT", "description": "BTC / USDT", "provider": "binance" }),
        json!({ "ticker": "ETH-USD", "provider": "coinbase", "prefix": "CB" }),
        json!({ "ticker": "SOLUSDT", "description": "SOL / USDT Perpetual" }),
        json!({ "ticker": "USDT" }),
        json!({ "ticker": "DOGE_PERP", "description": "" }),
        json!({ "ticker": "ES", "group": "ES", "prefix": "CME", "description": "E-mini S&P 500" }),
        json!({ "ticker": "ESZ6", "group": "ES", "prefix": "CME" }),
        json!({ "ticker": "ESH7", "group": "ES", "prefix": "CME", "default": true }),
        json!({ "ticker": "ESM7", "group": "es", "prefix": "cme" }),
        json!({ "ticker": "NQ", "group": "NQ", "provider": "cme" }),
        json!({ "ticker": "NQZ6", "group": "NQ", "provider": "cme", "default": true }),
        json!({ "ticker": "NQH7", "group": "NQ", "provider": "cme", "default": true }),
        json!({ "ticker": "CL", "group": "CL" }),
        json!({ "ticker": "ES", "group": "ES", "prefix": "ICE" }),
    ]
}

fn main() {
    let registry = ChartTypeRegistry::builtin();

    let timeframes = [
        "1", "5", "15", "60", "240", "D", "W", "M", "1D", "2W", "3M", "12M", "30S", "30s", "15m",
        "4h", "4H", "1d", "1w", "1M", " 60 ", "abc", "", "0", "1x", "999999", "D1", "1.5", "5 m",
        "-5", "007", "2Y",
    ];
    let timeframe_out: Vec<Value> = timeframes
        .iter()
        .map(|tf| json!({ "input": tf, "ms": timeframe_to_ms(tf) }))
        .collect();

    let hourly = walk(300, 1_709_251_200_000, 3_600_000);
    let view = heikin_ashi_full(&hourly);
    let stepped: Vec<Ohlcv> = hourly
        .iter()
        .enumerate()
        .map(|(i, raw)| heikin_ashi_next(raw, i.checked_sub(1).and_then(|p| view.get(p))))
        .collect();

    let tickers = [
        "AAPL",
        "AAPL;heikinashi",
        "AAPL;HEIKINASHI",
        "AAPL;HeikinAshi",
        "AAPL;standard",
        "A;B;heikinashi",
        ";heikinashi",
        "AAPL;",
        "AAPL;unknown",
        "",
        ";",
        "BINANCE:BTC;standard",
    ];
    let ticker_out: Vec<Value> = tickers
        .iter()
        .map(|t| {
            let parsed = parse_extended_ticker(&registry, t);
            json!({ "input": t, "symbol": parsed.symbol, "modifier": parsed.modifier, "hasTransform": parsed.transform.is_some() })
        })
        .collect();

    // 2023-11-20 daily: the last bar falls in 2025, so YTD and the long presets all bite.
    let daily = walk(500, 1_700_438_400_000, 86_400_000);
    let single = &daily[..1];
    let mut preset_out = Vec::new();
    for (name, set) in [
        ("daily", &daily[..]),
        ("hourly", &hourly[..]),
        ("single", single),
        ("empty", &[][..]),
    ] {
        for preset in VisibleRangePreset::ALL {
            let range = preset_to_range(preset, set).map(|r| json!({ "from": r.from, "to": r.to }));
            preset_out.push(json!({ "bars": name, "preset": preset.as_str(), "range": range }));
        }
    }

    let pool_json = symbol_pool();
    let pool: Vec<SymbolDescriptor> = pool_json
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    let symbol_out: Vec<Value> = pool
        .iter()
        .map(|d| {
            let base = base_of(d);
            json!({
                "ticker": d.ticker,
                "base": base,
                "icon": ledger_crypto_icon_url(&base),
                "isGroupRow": is_group_row(d),
                "groupKey": group_key_of(d),
                "members": group_members(&pool, d).iter().map(|m| m.ticker.clone()).collect::<Vec<_>>(),
                "defaultMember": default_member_of(&pool, d).map(|m| m.ticker.clone()),
            })
        })
        .collect();

    let docs = state_docs();
    let state_out: Vec<Value> = docs
        .iter()
        .map(|doc| {
            let sanitized = sanitize_state(doc).map(|state| {
                serde_json::from_str::<Value>(&encode_state(&state)).unwrap_or(Value::Null)
            });
            json!({ "doc": doc, "sanitized": sanitized })
        })
        .collect();

    let out = json!({
        "timeframes": timeframe_out,
        "heikinAshi": { "raw": bars(&hourly), "full": bars(&view), "stepped": bars(&stepped) },
        "tickers": ticker_out,
        "presets": { "bars": { "daily": bars(&daily), "hourly": bars(&hourly), "single": bars(single), "empty": [] }, "runs": preset_out },
        "symbols": { "pool": pool_json, "runs": symbol_out },
        "states": state_out,
    });
    println!("{out}");
}
