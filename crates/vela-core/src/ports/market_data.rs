//! The plain data types of the market-data port: facts about a symbol and the
//! bounded fetch window.
//!
//! Left out: the `MarketDataFeed` interface itself (`load`, `loadProgressive`,
//! `subscribe`, `loadRange`, `onUnresolved`, `destroy`). Its methods are
//! asynchronous (promises, abort signals, push subscriptions) and wait on the
//! data layer, so only the data it exchanges is here.

use crate::indicators::VisibleRange;
use crate::model::Ohlcv;
use crate::runtime::{AbortSignal, DataError, LocalBoxFuture, Unsubscribe};
use crate::visible_range::VisibleRangePreset;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The timeframes a source serves for a symbol (see [`SymbolInfo::timeframes`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SymbolTimeframes {
    /// The finest timeframe served. Every coarser one is served too, unless
    /// `list` is present.
    pub finest: String,
    /// The source's own frequency: the natural timeframe to show the symbol in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// When present, the EXHAUSTIVE list of timeframes served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<Vec<String>>,
}

/// Facts about one symbol, stated by its data source (`DataProvider.getSymbolInfo`):
/// the chart reads the typed keys below, and engines receive the whole record (e.g.
/// Pine `syminfo.*`). Free-form beyond them (`extra`): engines read the keys they
/// understand and synthesize a fallback for the rest. Every typed key but `ticker`
/// is optional, and an absent one means the usual behavior.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub ticker: String,
    /// The market's IANA time zone (`"America/New_York"`, `"Etc/UTC"`): the zone
    /// `session` and `session_extended` are written in, and the zone the "exchange"
    /// display time zone follows. Absent or empty means `"Etc/UTC"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// The regular trading hours, `HHMM-HHMM` in `timezone` (`"0930-1600"`; `2400`
    /// is the end of the day). Any other non-empty value marks a market with
    /// sessions (the regular/extended toggle, the calendar-driven market status)
    /// but shades nothing. `"24x7"`, empty or absent means a continuous market.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The whole extended tape, `HHMM-HHMM` in `timezone`, containing the `session`
    /// window (`"0400-2000"`). A span that wraps midnight (`"1700-1600"`) marks a
    /// market whose trading day runs from evening to evening. Absent means the
    /// whole civil day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_extended: Option<String>,
    /// The timeframes the source serves for this symbol. Timeframe strings use the
    /// uppercase canonical form: `"1D"` (or `"D"`), `"W"`, `"1M"` (or `"M"`),
    /// `"3M"`, `"12M"`, or bare minutes such as `"60"`. Never lowercase: `"1m"` is
    /// one minute, not one month. Absent means the provider's
    /// `info().supportedTimeframes`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeframes: Option<SymbolTimeframes>,
    /// `false` when the source has no volume data for this symbol: the chart then
    /// does not add its volume indicator on its own (one the user added or saved
    /// stays). Absent or `true` means volume as usual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<bool>,
    /// `false` when the source has ONE value per bar for this symbol, at every
    /// timeframe: open = high = low = close, so open, high and low carry no
    /// information (a macro-economic series). The chart then draws it with a style
    /// that reads the close (a line) whatever style is chosen, and its style
    /// pickers grey out the styles that need open, high and low (candles, bars,
    /// Heikin Ashi). Absent or `true` means normal OHLC bars. State it here: the
    /// chart never guesses it from the symbol's type or its bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ohlc: Option<bool>,
    /// Every other key the source states.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The facts about the chart's symbol that decide which price styles can draw it:
/// the neutral subset of [`SymbolInfo`] the chart applies once the symbol's info
/// lands and hands to a chart type's `supports` predicate. Each key mirrors the
/// `SymbolInfo` field of the same name; an absent key means the usual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFacts {
    /// `false`: one value per bar; open, high and low carry no information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ohlc: Option<bool>,
    /// `false`: the source has no volume for this symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<bool>,
}

/// A bounded fetch window (epoch ms). `from`/`to` inclusive-ish; `limit` caps the count.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BarRange {
    /// Oldest bar open-time to fetch from (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<i64>,
    /// Newest bar open-time to fetch to (defaults to "now" when omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<i64>,
    /// Max bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Trading session to serve (`"regular"` | `"extended"`): rides every request
    /// on markets that have sessions; absent means the provider's default. A
    /// provider without a session concept ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Which trading session the chart shows, on markets that have one:
/// `Regular` = RTH, `Extended` = pre/post-market included. The provider owns
/// the filtering; the flag rides every data request. Continuous markets
/// ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarketSession {
    Regular,
    Extended,
}

impl MarketSession {
    pub fn as_str(self) -> &'static str {
        match self {
            MarketSession::Regular => "regular",
            MarketSession::Extended => "extended",
        }
    }

    /// Narrow an untrusted string (persisted document, URL parameter) to a session.
    pub fn normalize(value: &str) -> Option<Self> {
        match value {
            "regular" => Some(MarketSession::Regular),
            "extended" => Some(MarketSession::Extended),
            _ => None,
        }
    }
}

/// The window to frame on the first paint: a preset name or an explicit range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InitialRange {
    Preset(VisibleRangePreset),
    Range(VisibleRange),
}

/// How the chart obtains its candles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarketConfig {
    /// The market symbol. A bare ticker (`BTCUSDT`) resolves against the
    /// registered providers in DECLARATION order (first one whose index lists
    /// it); an `EXCHANGE:` prefix (`coinbase:BTC-USD`, case-insensitive) pins
    /// the venue.
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub bars: Option<usize>,
    /// Absent = the provider default (`Regular` on session markets).
    pub session: Option<MarketSession>,
    /// Set when the initial view is known up front (a range chip, a shared
    /// link): the chart then loads the depth in ONE pass and paints the
    /// requested window straight away.
    pub visible_range: Option<InitialRange>,
    /// Offline bars instead of a provider; when set, no network fetch happens.
    pub data: Option<Vec<Ohlcv>>,
}

/// Receives each growing snapshot of a progressive load.
pub type BatchSink = Box<dyn FnMut(Vec<Ohlcv>)>;

/// Receives live forming-candle ticks.
pub type BarSink = Box<dyn FnMut(Ohlcv)>;

/// A symbol nothing registered can serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedSymbol {
    pub symbol: String,
    pub providers: Vec<String>,
}

/// The chart source of bars: a provider registry, a cache in front of one,
/// or an offline array.
///
/// An optional method of the original is a method here whose default answers
/// "not supported" (`None`); a caller then takes the same fallback the
/// original takes when the method is absent.
pub trait MarketDataFeed {
    /// Load the initial history.
    ///
    /// Every bar method fails with [`DataError::Unavailable`] when the source
    /// refuses the series for good. On the chart own market the chart then
    /// ends the load, stops live updates and reports the data as unavailable.
    /// Feeds should keep transient failures out of that channel.
    fn load(&self, cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>;

    /// Progressive load: emit growing snapshots while the source heals a cold
    /// symbol, resolve with the final answer. `None` (from the method, or as
    /// the resolved value) means the resolved source lacks the capability; the
    /// caller then runs its non-progressive paths untouched.
    fn load_progressive(
        &self,
        _cfg: &MarketConfig,
        _on_batch: BatchSink,
        _signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
        None
    }

    /// Subscribe to live forming-candle ticks.
    fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe;

    /// Symbol metadata for engines that need it; `None` means the engine synthesizes.
    fn symbol_info(&self, _cfg: &MarketConfig) -> Option<SymbolInfo> {
        None
    }

    /// Fetch a bounded range: a cache uses it to pull ONLY the uncached tail
    /// instead of re-downloading the whole window. `None` means no ranged
    /// support; the cache falls back to a full `load`.
    fn load_range(
        &self,
        _cfg: &MarketConfig,
        _range: &BarRange,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        None
    }

    /// Report that a symbol cannot be served by anything registered: the load
    /// is PARKED (it resumes if a capable provider registers later). `None`
    /// means the feed never parks.
    fn on_unresolved(&self, _callback: Box<dyn FnMut(&UnresolvedSymbol)>) -> Option<Unsubscribe> {
        None
    }

    /// Whether [`load_range`](Self::load_range) is implemented: the original
    /// tests `feed.loadRange` for presence (gap healing, deep chunked backfill,
    /// depth-only growth, the drawing series service). Asking starts no fetch.
    /// A feed that implements `load_range` MUST override this to `true`.
    fn supports_load_range(&self) -> bool {
        false
    }

    /// Whether [`load_progressive`](Self::load_progressive) is implemented (the
    /// original tests `feed.loadProgressive` for presence). Asking starts no
    /// fetch. A feed that implements `load_progressive` MUST override this to
    /// `true`.
    fn supports_load_progressive(&self) -> bool {
        false
    }

    /// Release feed-owned resources (parked waits, timers).
    fn destroy(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn symbol_info_keeps_unknown_keys_and_round_trips() {
        let raw = json!({
            "ticker": "AAPL", "timezone": "America/New_York", "session": "0930-1600",
            "timeframes": { "finest": "1", "list": ["1", "60", "1D"] },
            "ohlc": false, "pointvalue": 1, "description": "Apple"
        });
        let info: SymbolInfo = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(info.ticker, "AAPL");
        assert_eq!(info.ohlc, Some(false));
        assert_eq!(info.volume, None);
        assert_eq!(info.extra.get("description"), Some(&json!("Apple")));
        assert_eq!(
            info.timeframes
                .as_ref()
                .and_then(|t| t.list.as_ref())
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(serde_json::to_value(&info).unwrap(), raw);
    }

    #[test]
    fn symbol_info_needs_a_ticker() {
        assert!(serde_json::from_value::<SymbolInfo>(json!({ "timezone": "Etc/UTC" })).is_err());
        assert!(
            serde_json::from_value::<SymbolInfo>(json!({ "ticker": "X", "ohlc": "no" })).is_err()
        );
    }

    #[test]
    fn facts_and_ranges_default_to_absent() {
        assert_eq!(
            SymbolFacts::default(),
            serde_json::from_value(json!({})).unwrap()
        );
        let r: BarRange = serde_json::from_value(json!({ "from": 5, "limit": 100 })).unwrap();
        assert_eq!(
            (r.from, r.to, r.limit, r.session),
            (Some(5), None, Some(100), None)
        );
        assert_eq!(
            serde_json::to_value(BarRange::default()).unwrap(),
            json!({})
        );
    }
}
