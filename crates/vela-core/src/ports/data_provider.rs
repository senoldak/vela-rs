//! The data types of the `DataProvider` port: what a market-data source
//! advertises about itself and the symbols it serves.
//!
//! Left out: the `DataProvider` interface itself. Its methods (`getBars`,
//! `getBarsProgressive`, `listSymbols`, `getSymbolInfo`, `getCalendar`) are
//! asynchronous or take callbacks and abort signals, so they have no
//! synchronous Rust counterpart here.

use crate::model::Ohlcv;
use crate::runtime::{AbortSignal, DataError, LocalBoxFuture, Unsubscribe};

use super::market_data::{BarRange, BarSink, BatchSink, SymbolInfo};
use serde::{Deserialize, Serialize};

/// A descriptor for one symbol a provider serves. Powers the eager index built
/// at registration (for bare-symbol resolution) and symbol autocomplete.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolDescriptor {
    /// The ticker exactly as the provider expects it (`BTCUSDT`, `BTCUSDT.P`).
    pub ticker: String,
    /// Human-readable label (`Bitcoin / TetherUS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Instrument class, free-form (`crypto`, `futures`, `stock`).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The GROUP this row belongs to (futures: the product root, so `ES1!` and
    /// `ES2!` carry `group: "ES"`). The group's own row repeats the value in
    /// `ticker` with a distinguishing `type` and is NOT directly loadable:
    /// pickers fold members under it and load the member marked `default` when
    /// the group itself is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The member a picker loads when its whole GROUP is picked. At most one
    /// per group; the agreed fallback for zero-or-many is the group's FIRST
    /// listed member, so providers emit members in deliberate order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    /// The provider-side market (product class) serving this symbol, on
    /// providers whose markets differ in session shape. Absent where the
    /// market is unambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
    /// The instrument's LISTING-venue prefix (`NASDAQ`, `NYSE`, `AMEX`): a
    /// property of the SYMBOL, not of the provider. When declared, it is what
    /// pickers and legends display and what `PREFIX:TICKER` strings resolve
    /// against. Absent on venues where the provider IS the identity (crypto,
    /// fx), whose provider name prefixes those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Owning provider name, annotated by the registry aggregation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl SymbolDescriptor {
    /// A descriptor carrying only a ticker.
    pub fn new(ticker: impl Into<String>) -> Self {
        Self {
            ticker: ticker.into(),
            ..Self::default()
        }
    }
}

/// What a provider can do, so the registry and UI can reason without
/// provider-specific checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    /// Implements `listSymbols()` (enumeration, eager index and autocomplete).
    pub enumerate: bool,
    /// Implements `subscribe()` (true streaming; otherwise the feed polls).
    pub stream: bool,
    /// Implements `getSymbolInfo()` (per-symbol metadata for `syminfo.*`).
    pub symbol_info: bool,
}

/// Provider metadata, surfaced via `chart.data.providers()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    /// Stable id (the registration name, lower-cased).
    pub name: String,
    /// Display label (`Binance`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Whether the provider needs `configure()` before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_api_key: Option<bool>,
    /// Timeframes the provider serves, in the uppercase canonical form of
    /// `SymbolInfo.timeframes` (`1D`/`D`, `W`, `1M`/`M`, bare minutes such as
    /// `60`; never lowercase). Informational; a symbol's own `timeframes`
    /// overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_timeframes: Option<Vec<String>>,
    pub capabilities: ProviderCapabilities,
}

/// Ascending `(start, end)` pairs of open market time, in epoch ms.
pub type CalendarWindows = Vec<(i64, i64)>;

/// A calendar request: open-market windows over `[from, to)`.
#[derive(Clone, Debug, PartialEq)]
pub struct CalendarRange {
    pub from: i64,
    pub to: i64,
    /// Selects the window set (`"regular"` default; `"extended"` = the full tape).
    pub session: Option<String>,
}

/// A market-data source for ONE venue, neutral to Vela. A registry routes any
/// `name:SYMBOL` (or a bare `SYMBOL` it indexes) to it.
///
/// Only [`get_bars`](Self::get_bars) is required. Every other method is a
/// progressive capability: its default answers "not supported" (`None`), and
/// the registry degrades around it exactly as the original does around an
/// absent method. A provider that implements one returns `Some`.
pub trait DataProvider {
    /// Fetch bars for `ticker`/`timeframe`. `ticker` is exactly what the user
    /// typed minus any `provider:` prefix; any `.ext` suffix is KEPT (the
    /// provider owns its meaning). The newest bar is treated as the forming
    /// candle. Bars are sorted and de-duplicated by open time.
    ///
    /// [`DataError::Other`] is logged and read as an empty answer. To say the
    /// series is never served (a timeframe finer than the series offers), fail
    /// with [`DataError::Unavailable`]: the chart stops asking for it.
    fn get_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>;

    /// Progressive variant of `get_bars`, for sources that HEAL a cold symbol
    /// while answering. Every `on_batch` list (and the resolved final list) is
    /// the WHOLE answer so far: ascending, confirmed from its newest bar
    /// backward, each extending the previous one. `signal` aborts the stream:
    /// stop polling PROMPTLY and resolve with whatever is confirmed.
    fn get_bars_progressive(
        &self,
        _ticker: &str,
        _timeframe: &str,
        _range: &BarRange,
        _on_batch: BatchSink,
        _signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        None
    }

    /// Provider metadata. `None` means the registry synthesizes a record.
    fn info(&self) -> Option<ProviderInfo> {
        None
    }

    /// Enumerate the symbols this provider serves: drives the eager index
    /// built when the provider registers. `None` means no index; the provider
    /// can only be reached by an explicit `name:SYMBOL` prefix.
    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        None
    }

    /// Per-symbol facts. `None` means the engine synthesizes a fallback and
    /// the chart assumes the usual behavior.
    fn get_symbol_info(
        &self,
        _ticker: &str,
    ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
        None
    }

    /// The icon URL for one of THIS provider symbols. Called lazily per
    /// rendered row, so it must be cheap. The URL must be CORS-clean.
    fn resolve_symbol_icon(&self, _symbol: &SymbolDescriptor) -> Option<String> {
        None
    }

    /// Open a true live stream. Each `on_bar` call delivers the forming candle
    /// (or a freshly closed one). `None` means the feed polls `get_bars`
    /// instead. `session` names the trading session the chart is showing.
    fn subscribe(
        &self,
        _ticker: &str,
        _timeframe: &str,
        _on_bar: BarSink,
        _session: Option<&str>,
    ) -> Option<Unsubscribe> {
        None
    }

    /// Resolved market-calendar windows: ascending `(start, end)` pairs of
    /// OPEN market time, holidays and DST already applied by the source.
    /// `None` means the venue offers no calendar (continuous markets).
    fn get_calendar(
        &self,
        _ticker: &str,
        _range: &CalendarRange,
    ) -> Option<LocalBoxFuture<'static, Result<CalendarWindows, DataError>>> {
        None
    }

    /// Whether this provider implements [`subscribe`](Self::subscribe). A
    /// registry cannot find out by calling it (that would open a stream), so
    /// a provider that streams must say so here.
    fn supports_stream(&self) -> bool {
        false
    }

    /// Whether this provider implements [`get_symbol_info`](Self::get_symbol_info).
    fn supports_symbol_info(&self) -> bool {
        false
    }

    /// Apply runtime config (API keys). The default ignores it.
    fn configure(&self, _config: &serde_json::Value) {}

    /// Capabilities for ONE ticker, when they vary by instrument class within
    /// the venue. `None` means the provider-wide capabilities apply.
    fn capabilities_for(&self, _ticker: &str) -> Option<ProviderCapabilities> {
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn descriptor_json_matches_the_original_field_names() {
        let d = SymbolDescriptor {
            ticker: "ES1!".into(),
            kind: Some("futures".into()),
            group: Some("ES".into()),
            default: Some(true),
            prefix: Some("CME".into()),
            ..SymbolDescriptor::default()
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v,
            json!({ "ticker": "ES1!", "type": "futures", "group": "ES", "default": true, "prefix": "CME" })
        );
        let back: SymbolDescriptor = serde_json::from_value(v).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn a_bare_ticker_parses() {
        let d: SymbolDescriptor = serde_json::from_value(json!({ "ticker": "GDP" })).unwrap();
        assert_eq!(d, SymbolDescriptor::new("GDP"));
    }

    #[test]
    fn provider_info_uses_camel_case() {
        let info = ProviderInfo {
            name: "binance".into(),
            display_name: Some("Binance".into()),
            requires_api_key: None,
            supported_timeframes: Some(vec!["60".into(), "D".into()]),
            capabilities: ProviderCapabilities {
                enumerate: true,
                stream: false,
                symbol_info: true,
            },
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({
                "name": "binance",
                "displayName": "Binance",
                "supportedTimeframes": ["60", "D"],
                "capabilities": { "enumerate": true, "stream": false, "symbolInfo": true },
            })
        );
    }
}
