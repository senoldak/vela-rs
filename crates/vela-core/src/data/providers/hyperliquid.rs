//! Hyperliquid market-data provider, built on the public `info` API (no
//! third-party SDK, no API key). Serves USD-margined perpetuals (bare coins like
//! `BTC`), spot pairs (`PURR/USDC`) and the perpetuals of every builder-deployed
//! (HIP-3) perp dex (`xyz:TSLA`: the dex name, a colon, the coin, which is the
//! API's own coin name, so `hyperliquid:xyz:TSLA` is the qualified form), and
//! aggregates timeframes Hyperliquid does not serve natively (`45`, `180`, `360`).
//! Live ticks use a real WebSocket candle stream.
//!
//! History note: Hyperliquid only serves the most recent ~5000 candles per
//! interval (about 3.5d of 1m, ~7mo of 1h, daily back to listing). There is no
//! deeper backfill, so it is a live/recent-history venue rather than a
//! deep-history source.
//!
//! The original's constructor takes no options. Its WebSocket-availability check
//! in `subscribe` is gone: a [`Platform`] always offers sockets.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use super::common::{
    FetchError, Interval, Memo, SharedSink, SymbolFields, build_symbol_info, is_truthy, join_all,
    join2, js_num, now_ms, parse_json, parse_minutes, poll_bars, select_sub_tf, tail,
};
use crate::data::{base_of, ledger_crypto_icon_url};
use crate::model::Ohlcv;
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::{BarRange, BarSink, SymbolInfo};
use crate::runtime::{
    DataError, HttpRequest, LocalBoxFuture, Platform, Socket, SocketHandlers, TimerId, Unsubscribe,
};

pub use super::common::{aggregate, dedupe_sorted, normalize_tf};

const INFO_URL: &str = "https://api.hyperliquid.xyz/info";
const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";
/// If a candle socket opens but delivers no candle within this window, treat it as
/// non-delivering and fall back to polling. Covers a stream that connects (and ACKs
/// the subscription) but stays silent (e.g. a blocked/misconfigured endpoint), so live
/// updates never go silently dead. Generous enough not to mis-fire on an illiquid coin.
const STREAM_STALL_MS: f64 = 15_000.0;
/// Reconnect backoff after an unexpected socket close.
const STREAM_RECONNECT_MS: f64 = 2_000.0;
/// Keep-alive cadence of the idle socket (Hyperliquid answers with a pong).
const PING_MS: f64 = 30_000.0;
/// `spotMeta` token index of USDC, the main dex's collateral.
const USDC_TOKEN: f64 = 0.0;

/// Native intraday timeframes Hyperliquid serves (minutes): aggregation sub-candidates.
const NATIVE_MINUTES: [usize; 10] = [1, 3, 5, 15, 30, 60, 120, 240, 480, 720];
const SUPPORTED_TIMEFRAMES: [&str; 16] = [
    "1", "3", "5", "15", "30", "45", "60", "120", "180", "240", "360", "480", "720", "D", "W", "M",
];

/// Canonical timeframe to Hyperliquid interval string (native, no aggregation).
fn interval_of(tf: &str) -> Option<&'static str> {
    Some(match tf {
        "1" => "1m",
        "3" => "3m",
        "5" => "5m",
        "15" => "15m",
        "30" => "30m",
        "60" => "1h",
        "120" => "2h",
        "240" => "4h",
        "480" => "8h",
        "720" => "12h",
        "D" => "1d",
        "W" => "1w",
        "M" => "1M",
        _ => return None,
    })
}

/// Duration of one canonical-timeframe bar in ms (to turn a bar count into a time window).
fn tf_ms(tf: &str) -> i64 {
    match tf {
        "D" => 86_400_000,
        "W" => 604_800_000,
        "M" => 2_592_000_000, // month is about 30d (windowing only; not bar alignment)
        _ => {
            let digits = tf.bytes().take_while(u8::is_ascii_digit).count();
            match tf[..digits].parse::<i64>() {
                Ok(min) if min > 0 => min.saturating_mul(60_000),
                _ => 3_600_000,
            }
        }
    }
}

/// The dex and coin of a HIP-3 market (`xyz:TSLA` is dex `xyz`, name `TSLA`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DexCoin<'a> {
    pub dex: &'a str,
    pub name: &'a str,
}

/// The dex and coin of a HIP-3 market, or `None` for any other ticker.
pub fn split_dex_coin(coin: &str) -> Option<DexCoin<'_>> {
    let i = coin.find(':')?;
    if i == 0 || i == coin.len() - 1 || coin.contains('/') {
        return None;
    }
    Some(DexCoin {
        dex: &coin[..i],
        name: &coin[i + 1..],
    })
}

/// Map a Vela ticker to a Hyperliquid `coin`. Perps are bare names (`BTC`); spot
/// pairs are `BASE/QUOTE` (`PURR/USDC`); a builder-deployed (HIP-3) dex's perps are
/// `dex:COIN` (`xyz:TSLA`). Bare names are upper-cased for convenience; pair names
/// are passed through verbatim (they are the API coin). The API matches coins
/// case-sensitively, and dex names are lower-case, so a HIP-3 ticker gets a
/// lower-case dex and an upper-case coin; the provider swaps in the listed spelling
/// when it knows the market.
pub fn parse_coin(ticker: &str) -> String {
    let t = ticker.trim();
    if t.contains('/') {
        return t.to_string();
    }
    match split_dex_coin(t) {
        Some(DexCoin { dex, name }) => format!("{}:{}", dex.to_lowercase(), name.to_uppercase()),
        None => t.to_uppercase(),
    }
}

/// Map a single raw candle (`{ t, T, s, i, o, h, l, c, v, n }`, prices and volume as
/// strings) to a neutral bar (open-time in epoch ms).
pub fn candle_to_ohlcv(k: &Value) -> Ohlcv {
    Ohlcv::new(
        js_num(k.get("t")) as i64,
        js_num(k.get("o")),
        js_num(k.get("h")),
        js_num(k.get("l")),
        js_num(k.get("c")),
        Some(js_num(k.get("v"))),
    )
}

// The subsets of the `info` answers this provider reads.

struct PerpAsset {
    name: String,
    sz_decimals: Option<f64>,
    is_delisted: bool,
}

/// `meta`: the perp universe and, on a HIP-3 dex, the collateral token.
#[derive(Default)]
struct PerpMeta {
    universe: Vec<PerpAsset>,
    /// The spot token the dex's perps are margined in (its `index` in `spotMeta.tokens`).
    collateral_token: Option<f64>,
}

impl PerpMeta {
    fn parse(data: &Value) -> Self {
        let universe = data
            .get("universe")
            .and_then(Value::as_array)
            .map(|assets| {
                assets
                    .iter()
                    .filter_map(|u| {
                        Some(PerpAsset {
                            name: u.get("name")?.as_str()?.to_string(),
                            sz_decimals: u.get("szDecimals").and_then(Value::as_f64),
                            is_delisted: is_truthy(u.get("isDelisted")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            universe,
            collateral_token: data.get("collateralToken").and_then(Value::as_f64),
        }
    }

    fn find(&self, coin: &str) -> Option<&PerpAsset> {
        self.universe.iter().find(|u| u.name == coin)
    }
}

/// One `perpDexs` entry.
struct PerpDex {
    name: String,
    full_name: Option<String>,
}

struct SpotPair {
    name: String,
    tokens: Vec<f64>,
}

struct SpotToken {
    name: String,
    sz_decimals: Option<f64>,
    index: Option<f64>,
}

/// `spotMeta`: the spot pairs and the token table they index into.
#[derive(Default)]
struct SpotMeta {
    universe: Vec<SpotPair>,
    tokens: Vec<SpotToken>,
}

impl SpotMeta {
    fn parse(data: &Value) -> Self {
        let universe = data
            .get("universe")
            .and_then(Value::as_array)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|u| {
                        Some(SpotPair {
                            name: u.get("name")?.as_str()?.to_string(),
                            tokens: u
                                .get("tokens")
                                .and_then(Value::as_array)
                                .map(|t| t.iter().filter_map(Value::as_f64).collect())
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tokens = data
            .get("tokens")
            .and_then(Value::as_array)
            .map(|tokens| {
                tokens
                    .iter()
                    .filter_map(|t| {
                        Some(SpotToken {
                            name: t.get("name")?.as_str()?.to_string(),
                            sz_decimals: t.get("szDecimals").and_then(Value::as_f64),
                            index: t.get("index").and_then(Value::as_f64),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { universe, tokens }
    }

    /// The token at array position `i` (a pair's `tokens` entries are positions, not `index` values).
    fn token_at(&self, i: f64) -> Option<&SpotToken> {
        if i >= 0.0 && i.fract() == 0.0 {
            self.tokens.get(i as usize)
        } else {
            None
        }
    }
}

type Shared<T> = Result<Rc<T>, FetchError>;

struct Inner {
    platform: Rc<dyn Platform>,
    /// Cached perp `meta` (the universe is large; fetch once).
    meta: Memo<Shared<PerpMeta>>,
    /// Cached `spotMeta`.
    spot_meta: Memo<Shared<SpotMeta>>,
    /// Cached `perpDexs` (the HIP-3 dexes; the main dex is left out).
    perp_dexs: Memo<Shared<Vec<PerpDex>>>,
    /// Cached `meta` per HIP-3 dex name.
    dex_metas: RefCell<HashMap<String, Memo<Shared<PerpMeta>>>>,
    /// Upper-cased HIP-3 coin to its listed spelling, filled as dex metas arrive.
    hip3_coins: RefCell<HashMap<String, String>>,
    /// Cached symbol enumeration.
    symbols: Memo<Vec<SymbolDescriptor>>,
}

/// The Hyperliquid provider: a cheap-to-clone handle.
#[derive(Clone)]
pub struct HyperliquidProvider {
    inner: Rc<Inner>,
}

impl HyperliquidProvider {
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                meta: Memo::default(),
                spot_meta: Memo::default(),
                perp_dexs: Memo::default(),
                dex_metas: RefCell::new(HashMap::new()),
                hip3_coins: RefCell::new(HashMap::new()),
                symbols: Memo::default(),
            }),
        }
    }
}

impl DataProvider for HyperliquidProvider {
    fn supports_stream(&self) -> bool {
        true
    }

    fn supports_symbol_info(&self) -> bool {
        true
    }

    fn info(&self) -> Option<ProviderInfo> {
        Some(ProviderInfo {
            name: "hyperliquid".to_string(),
            display_name: Some("Hyperliquid".to_string()),
            requires_api_key: Some(false),
            supported_timeframes: Some(
                SUPPORTED_TIMEFRAMES.iter().map(|s| s.to_string()).collect(),
            ),
            capabilities: ProviderCapabilities {
                enumerate: true,
                stream: true,
                symbol_info: true,
            },
        })
    }

    fn get_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let bars =
            self.inner
                .clone()
                .bars(ticker.to_string(), timeframe.to_string(), range.clone());
        Box::pin(async move { Ok(bars.await) })
    }

    fn get_symbol_info(
        &self,
        ticker: &str,
    ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
        let inner = self.inner.clone();
        let ticker = ticker.to_string();
        Some(Box::pin(
            async move { Ok(inner.symbol_info(&ticker).await) },
        ))
    }

    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        let inner = self.inner.clone();
        let shared = self.inner.symbols.get(&self.inner.platform, move || {
            Box::pin(async move {
                let (perps, spot, hip3) = (inner.clone(), inner.clone(), inner.clone());
                let lists: Vec<LocalBoxFuture<'static, Vec<SymbolDescriptor>>> = vec![
                    Box::pin(async move { perps.list_perps().await.unwrap_or_default() }),
                    Box::pin(async move { spot.list_spot().await.unwrap_or_default() }),
                    Box::pin(async move { hip3.list_hip3().await.unwrap_or_default() }),
                ];
                join_all(lists).await.into_iter().flatten().collect()
            })
        });
        Some(Box::pin(async move { Ok(shared.await) }))
    }

    /// Predefined icon source for a crypto venue: the Ledger crypto-icon CDN, keyed by
    /// the BASE asset (the description's first segment, else the de-suffixed ticker; a
    /// HIP-3 ticker without its dex).
    fn resolve_symbol_icon(&self, symbol: &SymbolDescriptor) -> Option<String> {
        let has_description = symbol.description.as_deref().is_some_and(|d| !d.is_empty());
        let hip3 = if has_description {
            None
        } else {
            split_dex_coin(&symbol.ticker)
        };
        match hip3 {
            Some(DexCoin { name, .. }) => {
                ledger_crypto_icon_url(&base_of(&SymbolDescriptor::new(name)))
            }
            None => ledger_crypto_icon_url(&base_of(symbol)),
        }
    }

    fn subscribe(
        &self,
        ticker: &str,
        timeframe: &str,
        on_bar: BarSink,
        _session: Option<&str>,
    ) -> Option<Unsubscribe> {
        let inner = self.inner.clone();
        let coin = inner.coin_of(ticker);
        let on_bar = SharedSink::new(on_bar);
        // A native interval streams over the WebSocket; an aggregated timeframe polls getBars.
        Some(match interval_of(&normalize_tf(timeframe)) {
            Some(interval) => StreamCtx::start(inner, ticker, timeframe, coin, interval, on_bar),
            None => inner.poll(ticker, timeframe, on_bar),
        })
    }
}

/// The JSON string literal of `s`.
fn json_str(s: &str) -> String {
    Value::from(s).to_string()
}

/// 10^-decimals as a tick size (1 is 0.1, 3 is 0.001).
fn decimals_to_tick(decimals: f64) -> f64 {
    10f64.powf(-decimals)
}

/// A HIP-3 market's description: the coin, its quote and the dex that lists it
/// (`TSLA / USD Perpetual (XYZ)`).
fn hip3_description(name: &str, quote: &str, dex_name: &str) -> String {
    format!("{name} / {quote} Perpetual ({dex_name})")
}

/// A `SymbolInfo` record (shared by the perp and spot branches of `getSymbolInfo`).
fn symbol_info(
    ticker: &str,
    base: &str,
    quote: &str,
    kind: &str,
    description: String,
    mintick: f64,
) -> SymbolInfo {
    build_symbol_info(SymbolFields {
        venue: "HYPERLIQUID",
        ticker, // keep the original ticker (as Pine Script expects)
        description,
        kind,
        base: Some(Value::from(base)),
        quote: Some(Value::from(quote)),
        mintick,
    })
}

impl Inner {
    /// `getBars`, failing soft: an empty result parks/empties the chart with a
    /// warning rather than rejecting the parked load.
    fn bars(
        self: Rc<Self>,
        ticker: String,
        timeframe: String,
        range: BarRange,
    ) -> LocalBoxFuture<'static, Vec<Ohlcv>> {
        Box::pin(async move {
            match self.try_bars(&ticker, &timeframe, &range).await {
                Ok(bars) => bars,
                Err(e) => {
                    self.platform.warn(&format!(
                        "[vela] Hyperliquid: failed to fetch {ticker} {timeframe} — {e}"
                    ));
                    Vec::new()
                }
            }
        })
    }

    async fn try_bars(
        self: &Rc<Self>,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        let coin = self.resolve_coin(ticker).await;
        let tf = normalize_tf(timeframe);

        if let Some(interval) = interval_of(&tf) {
            let candles = self
                .fetch_candles(&coin, interval, tf_ms(&tf), range)
                .await?;
            let bars: Vec<Ohlcv> = candles.iter().map(candle_to_ohlcv).collect();
            return Ok(dedupe_sorted(&bars));
        }

        // Aggregation path: fetch a native sub-timeframe and combine.
        let target_min = parse_minutes(&tf);
        let sub_min = target_min.and_then(|t| select_sub_tf(&NATIVE_MINUTES, t));
        let (Some(target_min), Some(sub_min)) = (target_min, sub_min) else {
            self.platform.warn(&format!(
                "[vela] Hyperliquid: timeframe \"{timeframe}\" is not supported and cannot be aggregated."
            ));
            return Ok(Vec::new());
        };
        let ratio = target_min / sub_min;
        let sub_range = BarRange {
            limit: range.limit.map(|l| l * ratio + ratio),
            ..range.clone()
        };
        let interval = interval_of(&sub_min.to_string()).unwrap_or("1m");
        let candles = self
            .fetch_candles(&coin, interval, sub_min as i64 * 60_000, &sub_range)
            .await?;
        let sub: Vec<Ohlcv> = candles.iter().map(candle_to_ohlcv).collect();
        let agg = aggregate(&sub, target_min as i64 * 60_000);
        Ok(tail(agg, range.limit))
    }

    async fn symbol_info(self: &Rc<Self>, ticker: &str) -> Option<SymbolInfo> {
        let coin = self.resolve_coin(ticker).await;

        if let Some(DexCoin { dex, .. }) = split_dex_coin(&coin) {
            let meta = self.fetch_dex_meta(dex).await.ok();
            let perp = meta.as_ref()?.find(&coin)?;
            let (dex_entry, quote) = join2(
                self.find_dex(dex),
                self.collateral_name(meta.as_ref().and_then(|m| m.collateral_token)),
            )
            .await;
            let name = perp.name.get(dex.len() + 1..).unwrap_or("");
            // Same price rule as the main dex's perps (at most 6 - szDecimals decimals).
            let mintick = decimals_to_tick((6.0 - perp.sz_decimals.unwrap_or(2.0)).max(0.0));
            let dex_name = dex_entry
                .and_then(|d| d.full_name.filter(|n| !n.is_empty()))
                .unwrap_or_else(|| dex.to_string());
            return Some(symbol_info(
                ticker,
                name,
                &quote,
                "futures",
                hip3_description(name, &quote, &dex_name),
                mintick,
            ));
        }

        let meta = self.fetch_meta().await.ok();
        if let Some(perp) = meta.as_ref().and_then(|m| m.find(&coin)) {
            // HL perp prices: at most 5 significant figures, at most (6 - szDecimals)
            // decimals. Approximate the tick from the decimal bound: a hint for
            // `syminfo.mintick`, not an exact filter.
            let mintick = decimals_to_tick((6.0 - perp.sz_decimals.unwrap_or(2.0)).max(0.0));
            return Some(symbol_info(
                ticker,
                &coin,
                "USD",
                "futures",
                format!("{coin} / USD Perpetual"),
                mintick,
            ));
        }

        if coin.contains('/') {
            let spot = self.fetch_spot_meta().await.ok();
            let base_tok = spot.as_ref().and_then(|s| {
                let entry = s.universe.iter().find(|u| u.name == coin)?;
                s.token_at(entry.tokens.first().copied().unwrap_or(-1.0))
            });
            let mut parts = coin.split('/');
            let base = parts.next().unwrap_or(&coin);
            let quote = parts.next().unwrap_or("USDC");
            let mintick = decimals_to_tick(
                (8.0 - base_tok.and_then(|t| t.sz_decimals).unwrap_or(2.0)).max(0.0),
            );
            return Some(symbol_info(
                ticker,
                base,
                quote,
                "crypto",
                format!("{base} / {quote}"),
                mintick,
            ));
        }

        None
    }

    /// The API coin for a ticker: [`parse_coin`], with a known HIP-3 market's listed spelling.
    fn coin_of(&self, ticker: &str) -> String {
        let coin = parse_coin(ticker);
        let listed = self.hip3_coins.borrow().get(&coin.to_uppercase()).cloned();
        listed.unwrap_or(coin)
    }

    /// [`coin_of`](Self::coin_of), after loading a HIP-3 market's dex when its spelling isn't known yet.
    async fn resolve_coin(self: &Rc<Self>, ticker: &str) -> String {
        let coin = self.coin_of(ticker);
        let Some(DexCoin { dex, .. }) = split_dex_coin(&coin) else {
            return coin;
        };
        if self.hip3_coins.borrow().contains_key(&coin.to_uppercase()) {
            return coin;
        }
        // A failing dex request leaves the typed spelling in place.
        let _ = self.fetch_dex_meta(dex).await;
        self.coin_of(ticker)
    }

    async fn list_perps(self: &Rc<Self>) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let meta = self.fetch_meta().await?;
        Ok(meta
            .universe
            .iter()
            .filter(|u| !u.is_delisted)
            .map(|u| SymbolDescriptor {
                ticker: u.name.clone(),
                description: Some(format!("{} / USD Perpetual", u.name)),
                kind: Some("futures".to_string()),
                ..SymbolDescriptor::default()
            })
            .collect())
    }

    async fn list_spot(self: &Rc<Self>) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let meta = self.fetch_spot_meta().await?;
        let name_of = |i: Option<&f64>| {
            i.and_then(|i| meta.token_at(*i))
                .map(|t| t.name.as_str())
                .filter(|n| !n.is_empty())
        };
        Ok(meta
            .universe
            .iter()
            .map(|u| {
                let description = match (name_of(u.tokens.first()), name_of(u.tokens.get(1))) {
                    (Some(base), Some(quote)) => format!("{base} / {quote}"),
                    _ => u.name.clone(),
                };
                SymbolDescriptor {
                    ticker: u.name.clone(),
                    description: Some(description),
                    kind: Some("crypto".to_string()),
                    ..SymbolDescriptor::default()
                }
            })
            .collect())
    }

    /// Every HIP-3 dex's listed perps. A dex whose meta fails, or whose markets are all
    /// delisted (a halted dex), simply contributes nothing.
    async fn list_hip3(self: &Rc<Self>) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let dexes = self.fetch_perp_dexs().await?;
        let lists: Vec<LocalBoxFuture<'static, Vec<SymbolDescriptor>>> = (0..dexes.len())
            .map(|i| {
                let (inner, dexes) = (self.clone(), dexes.clone());
                Box::pin(async move { inner.list_dex(&dexes[i]).await.unwrap_or_default() })
                    as LocalBoxFuture<'static, Vec<SymbolDescriptor>>
            })
            .collect();
        Ok(join_all(lists).await.into_iter().flatten().collect())
    }

    async fn list_dex(self: &Rc<Self>, dex: &PerpDex) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let meta = self.fetch_dex_meta(&dex.name).await?;
        let quote = self.collateral_name(meta.collateral_token).await;
        let prefix = format!("{}:", dex.name);
        let dex_name = dex
            .full_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&dex.name);
        Ok(meta
            .universe
            .iter()
            .filter(|u| {
                !u.is_delisted && u.name.starts_with(&prefix) && u.name.len() > prefix.len()
            })
            .map(|u| SymbolDescriptor {
                ticker: u.name.clone(),
                description: Some(hip3_description(&u.name[prefix.len()..], &quote, dex_name)),
                kind: Some("futures".to_string()),
                ..SymbolDescriptor::default()
            })
            .collect())
    }

    /// The quote currency of a dex's perps, from its collateral token: USDC reads `USD` like the main dex.
    async fn collateral_name(self: &Rc<Self>, token: Option<f64>) -> String {
        let Some(token) = token.filter(|t| *t != USDC_TOKEN) else {
            return "USD".to_string();
        };
        let spot = self.fetch_spot_meta().await.ok();
        spot.and_then(|s| {
            s.tokens
                .iter()
                .find(|t| t.index == Some(token))
                .map(|t| t.name.clone())
        })
        .unwrap_or_else(|| "USD".to_string())
    }

    async fn find_dex(self: &Rc<Self>, name: &str) -> Option<PerpDexRef> {
        let dexes = self.fetch_perp_dexs().await.ok()?;
        dexes.iter().find(|d| d.name == name).map(|d| PerpDexRef {
            full_name: d.full_name.clone(),
        })
    }

    /// Fetch candles for a time window. Hyperliquid's `candleSnapshot` has no count
    /// parameter: it returns candles in `[startTime, endTime]` (capped to the most
    /// recent ~5000). A bar `limit` becomes a start offset; a `from` range (the
    /// cache's tail refresh) is used directly. The newest candle is the forming one.
    async fn fetch_candles(
        &self,
        coin: &str,
        interval: &str,
        interval_ms: i64,
        range: &BarRange,
    ) -> Result<Vec<Value>, FetchError> {
        let end = range.to.unwrap_or_else(|| now_ms(&self.platform));
        let start = range.from.unwrap_or_else(|| {
            let bars = range.limit.unwrap_or(500) as i64 + 2;
            end.saturating_sub(bars.saturating_mul(interval_ms))
        });
        let mut candles = self.candle_snapshot(coin, interval, start, end).await?;
        // Count-bounded (no explicit `from`): keep the most-recent `limit` bars.
        if let (None, Some(limit)) = (range.from, range.limit)
            && limit > 0
            && candles.len() > limit
        {
            candles.drain(..candles.len() - limit);
        }
        Ok(candles)
    }

    async fn candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<Value>, FetchError> {
        let body = format!(
            "{{\"type\":\"candleSnapshot\",\"req\":{{\"coin\":{},\"interval\":{},\"startTime\":{start_time},\"endTime\":{end_time}}}}}",
            json_str(coin),
            json_str(interval)
        );
        Ok(match self.post(body).await? {
            Value::Array(candles) => candles,
            _ => Vec::new(),
        })
    }

    /// Poll the forming candle (for aggregated timeframes).
    fn poll(self: &Rc<Self>, ticker: &str, timeframe: &str, on_bar: SharedSink) -> Unsubscribe {
        let inner = self.clone();
        let (ticker, timeframe) = (ticker.to_string(), timeframe.to_string());
        poll_bars(
            self.platform.clone(),
            move || {
                inner.clone().bars(
                    ticker.clone(),
                    timeframe.clone(),
                    BarRange {
                        limit: Some(2),
                        ..BarRange::default()
                    },
                )
            },
            on_bar,
        )
    }

    fn fetch_meta(self: &Rc<Self>) -> LocalBoxFuture<'static, Shared<PerpMeta>> {
        let inner = self.clone();
        self.meta.get(&self.platform, move || {
            Box::pin(async move {
                let data = inner.post("{\"type\":\"meta\"}".to_string()).await?;
                Ok(Rc::new(PerpMeta::parse(&data)))
            })
        })
    }

    fn fetch_spot_meta(self: &Rc<Self>) -> LocalBoxFuture<'static, Shared<SpotMeta>> {
        let inner = self.clone();
        self.spot_meta.get(&self.platform, move || {
            Box::pin(async move {
                let data = inner.post("{\"type\":\"spotMeta\"}".to_string()).await?;
                Ok(Rc::new(SpotMeta::parse(&data)))
            })
        })
    }

    /// The HIP-3 dexes: `perpDexs` lists the main dex first, as `null`, which is dropped.
    fn fetch_perp_dexs(self: &Rc<Self>) -> LocalBoxFuture<'static, Shared<Vec<PerpDex>>> {
        let inner = self.clone();
        self.perp_dexs.get(&self.platform, move || {
            Box::pin(async move {
                let data = inner.post("{\"type\":\"perpDexs\"}".to_string()).await?;
                let dexes = data
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|x| {
                                let name = x.get("name")?.as_str()?;
                                (!name.is_empty()).then(|| PerpDex {
                                    name: name.to_string(),
                                    full_name: x
                                        .get("fullName")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Rc::new(dexes))
            })
        })
    }

    fn fetch_dex_meta(self: &Rc<Self>, dex: &str) -> LocalBoxFuture<'static, Shared<PerpMeta>> {
        let memo = self
            .dex_metas
            .borrow_mut()
            .entry(dex.to_string())
            .or_default()
            .clone();
        let inner = self.clone();
        let dex = dex.to_string();
        memo.get(&self.platform, move || {
            Box::pin(async move {
                let body = format!("{{\"type\":\"meta\",\"dex\":{}}}", json_str(&dex));
                let data = inner.post(body).await?;
                let meta = PerpMeta::parse(&data);
                let mut coins = inner.hip3_coins.borrow_mut();
                for u in &meta.universe {
                    coins.insert(u.name.to_uppercase(), u.name.clone());
                }
                drop(coins);
                Ok(Rc::new(meta))
            })
        })
    }

    async fn post(&self, body: String) -> Result<Value, FetchError> {
        let res = self
            .platform
            .fetch(HttpRequest::post_json(INFO_URL, body))
            .await?;
        if !res.ok() {
            return Err(FetchError::new(format!("Hyperliquid HTTP {}", res.status)));
        }
        parse_json(&res)
    }
}

/// What `getSymbolInfo` reads of a `perpDexs` entry.
struct PerpDexRef {
    full_name: Option<String>,
}

/// One live candle subscription. A WebSocket candle stream that reconnects on an
/// unexpected close until unsubscribed. A stall watchdog falls the subscription back
/// to polling if the socket opens but never delivers a candle (a blocked or silent
/// stream), so live updates never go silently dead.
struct StreamCtx {
    provider: Rc<Inner>,
    ticker: String,
    timeframe: String,
    coin: String,
    interval: &'static str,
    on_bar: SharedSink,
    closed: Cell<bool>,
    ws: RefCell<Option<Rc<dyn Socket>>>,
    ping: RefCell<Option<Interval>>,
    reconnect: Cell<Option<TimerId>>,
    stall: Cell<Option<TimerId>>,
    /// Set once the stream is declared non-delivering and polling has taken over.
    polling: Cell<bool>,
    stop_polling: RefCell<Option<Unsubscribe>>,
}

impl StreamCtx {
    fn start(
        provider: Rc<Inner>,
        ticker: &str,
        timeframe: &str,
        coin: String,
        interval: &'static str,
        on_bar: SharedSink,
    ) -> Unsubscribe {
        let ctx = Rc::new(StreamCtx {
            provider,
            ticker: ticker.to_string(),
            timeframe: timeframe.to_string(),
            coin,
            interval,
            on_bar,
            closed: Cell::new(false),
            ws: RefCell::new(None),
            ping: RefCell::new(None),
            reconnect: Cell::new(None),
            stall: Cell::new(None),
            polling: Cell::new(false),
            stop_polling: RefCell::new(None),
        });
        ctx.open();
        Box::new(move || {
            ctx.closed.set(true);
            ctx.clear_stall();
            ctx.clear_ping();
            if let Some(id) = ctx.reconnect.take() {
                ctx.provider.platform.clear_timeout(id);
            }
            let stop = ctx.stop_polling.borrow_mut().take();
            if let Some(stop) = stop {
                stop();
            }
            let ws = ctx.ws.borrow_mut().take();
            if let Some(ws) = ws {
                ws.close();
            }
        })
    }

    fn clear_stall(&self) {
        if let Some(id) = self.stall.take() {
            self.provider.platform.clear_timeout(id);
        }
    }

    fn clear_ping(&self) {
        let ping = self.ping.borrow_mut().take();
        if let Some(ping) = ping {
            ping.clear();
        }
    }

    /// Tear down the socket and switch to polling: covers an open-but-silent stream.
    fn fall_to_polling(self: &Rc<Self>) {
        if self.closed.get() || self.polling.get() {
            return;
        }
        self.clear_stall();
        self.clear_ping();
        if let Some(id) = self.reconnect.take() {
            self.provider.platform.clear_timeout(id);
        }
        let ws = self.ws.borrow_mut().take();
        self.provider.platform.warn(&format!(
            "[vela] Hyperliquid: {} {} stream delivered no data; falling back to polling.",
            self.ticker, self.timeframe
        ));
        // Polling is engaged before the socket closes, so its close handler cannot reconnect.
        self.polling.set(true);
        let stop = self
            .provider
            .poll(&self.ticker, &self.timeframe, self.on_bar.clone());
        *self.stop_polling.borrow_mut() = Some(stop);
        if let Some(ws) = ws {
            ws.close();
        }
    }

    fn open(self: &Rc<Self>) {
        if self.closed.get() || self.polling.get() {
            return;
        }
        let platform = self.provider.platform.clone();
        let on_open = {
            let ctx = self.clone();
            move || {
                let ws = ctx.ws.borrow().clone();
                if let Some(ws) = ws {
                    ws.send(&format!(
                        "{{\"method\":\"subscribe\",\"subscription\":{{\"type\":\"candle\",\"coin\":{},\"interval\":{}}}}}",
                        json_str(&ctx.coin),
                        json_str(ctx.interval)
                    ));
                }
                // Keep the idle socket alive (HL replies with a pong).
                ctx.clear_ping();
                let pinger = ctx.clone();
                *ctx.ping.borrow_mut() = Some(Interval::start(
                    &ctx.provider.platform,
                    PING_MS,
                    move || {
                        let ws = pinger.ws.borrow().clone();
                        if let Some(ws) = ws {
                            ws.send("{\"method\":\"ping\"}");
                        }
                    },
                ));
            }
        };
        let on_message = {
            let ctx = self.clone();
            move |text: &str| {
                if ctx.closed.get() || ctx.polling.get() {
                    return; // unsubscribed, or already fell back to polling
                }
                // Non-JSON and unrelated frames are ignored.
                let Ok(msg) = serde_json::from_str::<Value>(text) else {
                    return;
                };
                if msg.get("channel").and_then(Value::as_str) != Some("candle") {
                    return;
                }
                let Some(data) = msg
                    .get("data")
                    .filter(|d| d.get("s").and_then(Value::as_str) == Some(ctx.coin.as_str()))
                else {
                    return;
                };
                ctx.clear_stall();
                ctx.on_bar.emit(candle_to_ohlcv(data));
            }
        };
        let on_close = {
            let ctx = self.clone();
            move || {
                ctx.clear_ping();
                if !ctx.closed.get() && !ctx.polling.get() {
                    // transient drop: retry
                    let again = ctx.clone();
                    let id = ctx
                        .provider
                        .platform
                        .set_timeout(STREAM_RECONNECT_MS, Box::new(move || again.open()));
                    ctx.reconnect.set(Some(id));
                }
            }
        };
        let on_error = {
            let ctx = self.clone();
            move || {
                let ws = ctx.ws.borrow().clone();
                if let Some(ws) = ws {
                    ws.close(); // already closed: the close handler reconnects
                }
            }
        };
        let ws = platform.open_socket(
            WS_URL,
            SocketHandlers {
                on_open: Some(Box::new(on_open)),
                on_message: Some(Box::new(on_message)),
                on_error: Some(Box::new(on_error)),
                on_close: Some(Box::new(on_close)),
            },
        );
        *self.ws.borrow_mut() = Some(ws);
        let ctx = self.clone();
        let id = platform.set_timeout(
            STREAM_STALL_MS, // watchdog: data must arrive
            Box::new(move || ctx.fall_to_polling()),
        );
        self.stall.set(Some(id));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use crate::runtime::{HttpResponse, NetError};

    const MIN: i64 = 60_000;

    fn setup() -> (ManualPlatform, HyperliquidProvider) {
        let platform = ManualPlatform::new(1_700_000_000_000.0);
        let provider = HyperliquidProvider::new(platform.handle());
        (platform, provider)
    }

    /// A raw Hyperliquid candle (prices/volume as strings, like the live API).
    fn candle(t_min: i64, interval_min: i64, o: &str, h: &str, l: &str, c: &str, s: &str) -> Value {
        let t = t_min * MIN;
        json!({ "t": t, "T": t + interval_min * MIN - 1, "s": s, "i": format!("{interval_min}m"),
                "o": o, "h": h, "l": l, "c": c, "v": "1", "n": 1 })
    }

    fn candle60(t_min: i64) -> Value {
        candle(t_min, 60, "100", "110", "90", "101", "BTC")
    }

    fn bar(time: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Ohlcv {
        Ohlcv::new(time, o, h, l, c, Some(v))
    }

    fn limit(n: usize) -> BarRange {
        BarRange {
            limit: Some(n),
            ..BarRange::default()
        }
    }

    /// Route every POST /info by its JSON body. A `{ "__notok": true, "status": n }`
    /// payload answers with that HTTP status instead.
    fn stub_info(platform: &ManualPlatform, route: impl Fn(&Value) -> Value + 'static) {
        platform.on_fetch(move |req| {
            let body: Value = serde_json::from_str(req.body.as_deref().unwrap_or("{}")).unwrap();
            let out = route(&body);
            if out.get("__notok").is_some() {
                let status = out["status"].as_u64().unwrap_or(500) as u16;
                return Some(Ok(HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: "{}".into(),
                }));
            }
            Some(Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: out.to_string(),
            }))
        });
    }

    fn calls(platform: &ManualPlatform) -> Vec<Value> {
        platform
            .requests()
            .iter()
            .map(|r| serde_json::from_str(r.body.as_deref().unwrap_or("{}")).unwrap())
            .collect()
    }

    fn snapshot(platform: &ManualPlatform) -> Value {
        calls(platform)
            .into_iter()
            .find(|c| c["type"] == "candleSnapshot")
            .unwrap()
    }

    fn get_bars(
        platform: &ManualPlatform,
        provider: &HyperliquidProvider,
        ticker: &str,
        tf: &str,
        range: BarRange,
    ) -> Vec<Ohlcv> {
        platform
            .block_on(provider.get_bars(ticker, tf, &range))
            .unwrap()
    }

    // ── pure helpers ──

    #[test]
    fn normalize_tf_maps_aliases_to_canonical_keys() {
        assert_eq!(normalize_tf("1h"), "60");
        assert_eq!(normalize_tf("4h"), "240");
        assert_eq!(normalize_tf("1d"), "D");
        assert_eq!(normalize_tf("45m"), "45");
        assert_eq!(normalize_tf("D"), "D");
        assert_eq!(normalize_tf("60"), "60"); // already canonical
    }

    #[test]
    fn parse_coin_upper_cases_bare_perps_and_passes_spot_pairs_through() {
        assert_eq!(parse_coin("btc"), "BTC");
        assert_eq!(parse_coin(" eth "), "ETH");
        assert_eq!(parse_coin("PURR/USDC"), "PURR/USDC");
        assert_eq!(parse_coin("purr/usdc"), "purr/usdc"); // pair names are the API coin, verbatim
    }

    #[test]
    fn a_hip3_ticker_keeps_its_dex_lower_case_and_the_coin_upper_case() {
        assert_eq!(parse_coin("xyz:TSLA"), "xyz:TSLA");
        assert_eq!(parse_coin("XYZ:tsla"), "xyz:TSLA");
    }

    #[test]
    fn split_dex_coin_recognizes_only_dex_colon_coin() {
        assert_eq!(
            split_dex_coin("xyz:TSLA"),
            Some(DexCoin {
                dex: "xyz",
                name: "TSLA"
            })
        );
        assert_eq!(split_dex_coin(":TSLA"), None);
        assert_eq!(split_dex_coin("xyz:"), None);
        assert_eq!(split_dex_coin("BTC"), None);
        assert_eq!(split_dex_coin("a:B/USDC"), None);
    }

    #[test]
    fn candle_to_ohlcv_parses_string_prices() {
        let k = json!({ "t": 1000, "T": 4999, "s": "BTC", "i": "1m", "o": "10", "h": "12", "l": "9", "c": "11", "v": "100", "n": 4 });
        assert_eq!(candle_to_ohlcv(&k), bar(1000, 10.0, 12.0, 9.0, 11.0, 100.0));
    }

    #[test]
    fn dedupe_sorted_sorts_by_time_and_keeps_the_last_bar_per_time() {
        let a = bar(2, 1.0, 1.0, 1.0, 1.0, 0.0);
        let b = bar(1, 1.0, 1.0, 1.0, 1.0, 0.0);
        let b2 = bar(1, 9.0, 9.0, 9.0, 9.0, 0.0);
        let out = dedupe_sorted(&[a, b, b2]);
        assert_eq!(out.iter().map(|x| x.time).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(out[0].open, 9.0); // incoming b2 won
    }

    #[test]
    fn aggregate_buckets_ascending_sub_candles_into_epoch_aligned_periods() {
        let sub: Vec<Ohlcv> = [0, 15, 30, 45, 60, 75]
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let i = i as f64;
                bar(m * MIN, 100.0 + i, 110.0 + i, 90.0 + i, 101.0 + i, 1.0)
            })
            .collect();
        let out = aggregate(&sub, 45 * MIN);
        assert_eq!(
            out.iter().map(|x| x.time).collect::<Vec<_>>(),
            vec![0, 45 * MIN]
        );
        assert_eq!(out[0], bar(0, 100.0, 112.0, 90.0, 103.0, 3.0));
    }

    #[test]
    fn bar_durations_for_windowing() {
        assert_eq!(tf_ms("D"), 86_400_000);
        assert_eq!(tf_ms("M"), 2_592_000_000);
        assert_eq!(tf_ms("60"), 3_600_000);
        assert_eq!(tf_ms("15"), 900_000);
        assert_eq!(tf_ms("weird"), 3_600_000);
        assert_eq!(tf_ms("0"), 3_600_000);
    }

    // ── getBars ──

    #[test]
    fn a_native_timeframe_is_fetched_directly_and_mapped() {
        let (platform, provider) = setup();
        let rows: Vec<Value> = [0, 60, 120].iter().map(|m| candle60(*m)).collect();
        stub_info(&platform, move |b| {
            if b["type"] == "candleSnapshot" {
                json!(rows)
            } else {
                json!([])
            }
        });

        let bars = get_bars(&platform, &provider, "BTC", "1h", limit(3));
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0], bar(0, 100.0, 110.0, 90.0, 101.0, 1.0));
        let snap = snapshot(&platform);
        assert_eq!(snap["req"]["coin"], "BTC");
        assert_eq!(snap["req"]["interval"], "1h");
        // limit 3 asks for the last (3 + 2) hours, ending now
        assert_eq!(snap["req"]["endTime"], 1_700_000_000_000_i64);
        assert_eq!(
            snap["req"]["startTime"],
            1_700_000_000_000_i64 - 5 * 3_600_000
        );
        let req = &platform.requests()[0];
        assert_eq!(req.url, "https://api.hyperliquid.xyz/info");
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.headers,
            vec![("Content-Type".to_string(), "application/json".to_string())]
        );
    }

    #[test]
    fn the_snapshot_body_keeps_the_original_field_order() {
        let (platform, provider) = setup();
        stub_info(&platform, |_| json!([]));
        get_bars(&platform, &provider, "BTC", "1h", limit(1));
        assert_eq!(
            platform.requests()[0].body.as_deref(),
            Some(
                "{\"type\":\"candleSnapshot\",\"req\":{\"coin\":\"BTC\",\"interval\":\"1h\",\"startTime\":1699989200000,\"endTime\":1700000000000}}"
            )
        );
    }

    #[test]
    fn an_unsupported_timeframe_aggregates_a_native_sub_timeframe() {
        let (platform, provider) = setup();
        let sub: Vec<Value> = [0, 15, 30, 45, 60, 75]
            .iter()
            .map(|m| candle(*m, 15, "100", "110", "90", "101", "BTC"))
            .collect();
        stub_info(&platform, move |b| {
            if b["type"] == "candleSnapshot" {
                json!(sub)
            } else {
                json!([])
            }
        });
        let bars = get_bars(&platform, &provider, "BTC", "45", limit(2));
        assert_eq!(snapshot(&platform)["req"]["interval"], "15m");
        assert_eq!(
            bars.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![0, 45 * MIN]
        );
    }

    #[test]
    fn a_count_bound_fetch_keeps_only_the_most_recent_bars() {
        let (platform, provider) = setup();
        let rows: Vec<Value> = (0..10).map(|i| candle60(i * 60)).collect();
        stub_info(&platform, move |_| json!(rows));
        let bars = get_bars(&platform, &provider, "BTC", "1h", limit(4));
        assert_eq!(bars.len(), 4);
        assert_eq!(bars[0].time, 6 * 3_600_000);
    }

    #[test]
    fn a_spot_pair_is_passed_through_as_the_api_coin() {
        let (platform, provider) = setup();
        stub_info(&platform, |b| {
            if b["type"] == "candleSnapshot" {
                json!([candle(0, 60, "1", "1", "1", "1", "PURR/USDC")])
            } else {
                json!([])
            }
        });
        get_bars(&platform, &provider, "PURR/USDC", "1h", limit(1));
        assert_eq!(snapshot(&platform)["req"]["coin"], "PURR/USDC");
    }

    #[test]
    fn range_from_is_used_directly_instead_of_a_count_window() {
        let (platform, provider) = setup();
        stub_info(&platform, |_| json!([candle60(100)]));
        let range = BarRange {
            from: Some(5_000_000),
            ..BarRange::default()
        };
        get_bars(&platform, &provider, "BTC", "1h", range);
        assert_eq!(snapshot(&platform)["req"]["startTime"], 5_000_000);
    }

    #[test]
    fn a_failed_request_fails_soft_with_a_warning() {
        let (platform, provider) = setup();
        stub_info(&platform, |_| json!({ "__notok": true, "status": 503 }));
        assert!(get_bars(&platform, &provider, "BTC", "1h", limit(5)).is_empty());
        let warnings = platform.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("failed to fetch"));
        assert!(warnings[0].contains("Hyperliquid HTTP 503"));
    }

    #[test]
    fn malformed_json_and_a_network_error_fail_soft() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| {
            Some(Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: "<html>".into(),
            }))
        });
        assert!(get_bars(&platform, &provider, "BTC", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].contains("failed to fetch BTC 1h"));

        let (platform, provider) = setup();
        platform.on_fetch(|_| Some(Err(NetError::Failed("offline".into()))));
        assert!(get_bars(&platform, &provider, "BTC", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].ends_with("— offline"));
    }

    #[test]
    fn a_non_array_snapshot_reads_as_empty() {
        let (platform, provider) = setup();
        stub_info(&platform, |_| json!({ "error": "bad" }));
        assert!(get_bars(&platform, &provider, "BTC", "1h", limit(5)).is_empty());
        assert!(platform.warnings().is_empty());
    }

    #[test]
    fn a_timeframe_that_cannot_be_aggregated_warns() {
        let (platform, provider) = setup();
        stub_info(&platform, |_| json!([]));
        assert!(get_bars(&platform, &provider, "BTC", "x", limit(3)).is_empty());
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Hyperliquid: timeframe \"x\" is not supported and cannot be aggregated."]
        );
    }

    // ── enumeration and symbol info ──

    fn meta() -> Value {
        json!({ "universe": [
            { "name": "BTC", "szDecimals": 5, "maxLeverage": 40 },
            { "name": "OLD", "szDecimals": 1, "maxLeverage": 5, "isDelisted": true },
            { "name": "ETH", "szDecimals": 4, "maxLeverage": 25 },
        ] })
    }

    fn spot_meta() -> Value {
        json!({
            "universe": [{ "tokens": [1, 0], "name": "PURR/USDC", "index": 0, "isCanonical": true }],
            "tokens": [{ "name": "USDC", "szDecimals": 8, "index": 0 }, { "name": "PURR", "szDecimals": 0, "index": 1 }],
        })
    }

    fn route(b: &Value) -> Value {
        match b["type"].as_str() {
            Some("meta") => meta(),
            Some("spotMeta") => spot_meta(),
            _ => json!([]),
        }
    }

    #[test]
    fn list_symbols_enumerates_perps_without_delisted_plus_spot() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let tickers: Vec<&str> = list.iter().map(|s| s.ticker.as_str()).collect();
        assert_eq!(tickers, vec!["BTC", "ETH", "PURR/USDC"]);
        assert_eq!(list[0].kind.as_deref(), Some("futures"));
        assert_eq!(list[0].description.as_deref(), Some("BTC / USD Perpetual"));
        assert_eq!(list[2].description.as_deref(), Some("PURR / USDC"));
        assert_eq!(list[2].kind.as_deref(), Some("crypto"));
    }

    #[test]
    fn a_spot_pair_with_unresolvable_tokens_is_described_by_its_name() {
        let (platform, provider) = setup();
        stub_info(&platform, |b| {
            if b["type"] == "spotMeta" {
                json!({ "universe": [{ "tokens": [7, 9], "name": "@7" }, { "name": "@8" }], "tokens": [] })
            } else {
                route(b)
            }
        });
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let spot: Vec<_> = list.iter().filter(|s| s.ticker.starts_with('@')).collect();
        assert_eq!(spot[0].description.as_deref(), Some("@7"));
        assert_eq!(spot[1].description.as_deref(), Some("@8"));
    }

    #[test]
    fn the_symbol_list_is_fetched_once() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        let first = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let before = platform.requests().len();
        let second = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        assert_eq!(first, second);
        assert_eq!(platform.requests().len(), before);
    }

    #[test]
    fn symbol_info_builds_a_perp_record_with_a_tick_from_sz_decimals() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        let info = platform
            .block_on(provider.get_symbol_info("BTC").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.ticker, "BTC");
        assert_eq!(info.extra["type"], json!("futures"));
        assert_eq!(info.extra["basecurrency"], json!("BTC"));
        assert_eq!(info.extra["currency"], json!("USD"));
        assert_eq!(info.extra["prefix"], json!("HYPERLIQUID"));
        assert_eq!(info.extra["tickerid"], json!("HYPERLIQUID:BTC"));
        assert_eq!(info.extra["description"], json!("BTC / USD Perpetual"));
        assert!((info.extra["mintick"].as_f64().unwrap() - 0.1).abs() < 1e-12); // 6 - szDecimals(5) = 1 decimal
        assert_eq!(info.extra["pricescale"], json!(10));
        assert_eq!(info.timezone.as_deref(), Some("Etc/UTC"));
        assert_eq!(info.session.as_deref(), Some("24x7"));
    }

    #[test]
    fn symbol_info_builds_a_spot_record_from_the_token_pair() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        let info = platform
            .block_on(provider.get_symbol_info("PURR/USDC").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.extra["type"], json!("crypto"));
        assert_eq!(info.extra["basecurrency"], json!("PURR"));
        assert_eq!(info.extra["currency"], json!("USDC"));
        assert_eq!(info.extra["description"], json!("PURR / USDC"));
        // PURR has szDecimals 0: 8 decimals
        assert!((info.extra["mintick"].as_f64().unwrap() - 1e-8).abs() < 1e-20);
        assert_eq!(info.extra["pricescale"], json!(100_000_000));
    }

    #[test]
    fn symbol_info_is_absent_for_an_unknown_bare_coin_or_a_failed_meta() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        assert!(
            platform
                .block_on(provider.get_symbol_info("NOPE").unwrap())
                .unwrap()
                .is_none()
        );
        // an unlisted spot pair still gets a record from its name, as the original does
        let info = platform
            .block_on(provider.get_symbol_info("FOO/USDC").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.extra["basecurrency"], json!("FOO"));
        assert!((info.extra["mintick"].as_f64().unwrap() - 1e-6).abs() < 1e-18); // default szDecimals 2

        let (platform, provider) = setup();
        stub_info(&platform, |_| json!({ "__notok": true, "status": 500 }));
        assert!(
            platform
                .block_on(provider.get_symbol_info("BTC").unwrap())
                .unwrap()
                .is_none()
        );
    }

    // ── subscribe ──

    type Bars = Rc<RefCell<Vec<Ohlcv>>>;

    fn collector() -> (Bars, BarSink) {
        let bars: Bars = Rc::new(RefCell::new(Vec::new()));
        let sink = bars.clone();
        (bars, Box::new(move |b| sink.borrow_mut().push(b)))
    }

    #[test]
    fn a_native_interval_streams_over_the_websocket() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "1h", sink, None).unwrap();
        let sockets = platform.sockets();
        assert_eq!(sockets.len(), 1); // opened synchronously
        let ws = sockets[0].clone();
        assert_eq!(ws.url, "wss://api.hyperliquid.xyz/ws");
        ws.emit_open();
        assert_eq!(
            ws.sent(),
            vec![
                "{\"method\":\"subscribe\",\"subscription\":{\"type\":\"candle\",\"coin\":\"BTC\",\"interval\":\"1h\"}}"
            ]
        );
        ws.emit_message(&json!({ "channel": "candle", "data": { "t": 1000, "s": "BTC", "i": "1h", "o": "10", "h": "12", "l": "9", "c": "11", "v": "3", "n": 1 } }).to_string());
        assert_eq!(*bars.borrow(), vec![bar(1000, 10.0, 12.0, 9.0, 11.0, 3.0)]);
        // other channels, other coins, and junk are ignored
        ws.emit_message(&json!({ "channel": "pong" }).to_string());
        ws.emit_message(
            &json!({ "channel": "subscriptionResponse", "data": { "method": "subscribe" } })
                .to_string(),
        );
        ws.emit_message(
            &json!({ "channel": "candle", "data": { "t": 5, "s": "ETH" } }).to_string(),
        );
        ws.emit_message("not json");
        assert_eq!(bars.borrow().len(), 1);
        unsub();
        assert!(ws.is_closed()); // closed on unsubscribe
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn an_open_socket_pings_every_thirty_seconds() {
        let (platform, provider) = setup();
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        ws.emit_message(
            &json!({ "channel": "candle", "data": { "t": 1, "s": "BTC" } }).to_string(),
        );
        platform.advance(29_999.0);
        assert_eq!(ws.sent().len(), 1);
        platform.advance(1.0);
        assert_eq!(ws.sent()[1], "{\"method\":\"ping\"}");
        platform.advance(30_000.0);
        assert_eq!(ws.sent().len(), 3);
        ws.emit_close(); // the ping stops with the socket
        platform.advance(1_999.0);
        unsub();
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn an_aggregated_timeframe_polls_get_bars() {
        let (platform, provider) = setup();
        let sub: Vec<Value> = [0, 15, 30]
            .iter()
            .map(|m| candle(*m, 15, "100", "110", "90", "101", "BTC"))
            .collect();
        stub_info(&platform, move |b| {
            if b["type"] == "candleSnapshot" {
                json!(sub)
            } else {
                json!([])
            }
        });
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "45", sink, None).unwrap();
        assert!(platform.sockets().is_empty());
        platform.advance(2_999.0);
        assert!(bars.borrow().is_empty());
        platform.advance(2.0);
        let first = bars.borrow().len();
        assert!(first > 0);
        platform.advance(3_000.0);
        assert!(bars.borrow().len() > first);
        unsub();
        let after = bars.borrow().len();
        platform.advance(10_000.0);
        assert_eq!(bars.borrow().len(), after);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_silent_stream_falls_back_to_polling_after_the_stall_watchdog() {
        let (platform, provider) = setup();
        stub_info(&platform, |b| {
            if b["type"] == "candleSnapshot" {
                json!([candle60(0)])
            } else {
                json!([])
            }
        });
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "1h", sink, None).unwrap();
        assert_eq!(platform.sockets().len(), 1);
        let ws = platform.sockets()[0].clone();
        ws.emit_open(); // opens, never delivers a candle frame
        platform.advance(14_999.0);
        assert!(platform.warnings().is_empty());
        assert!(!ws.is_closed());
        platform.advance(1.0); // the 15 s watchdog fires
        assert!(ws.is_closed()); // the dead socket was closed
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Hyperliquid: BTC 1h stream delivered no data; falling back to polling."]
        );
        platform.advance(3_000.0); // a poll cycle
        assert!(!bars.borrow().is_empty());
        platform.advance(60_000.0);
        assert_eq!(platform.sockets().len(), 1); // no reconnect after the fallback
        unsub();
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_candle_clears_the_watchdog() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        platform.advance(10_000.0);
        ws.emit_message(&json!({ "channel": "candle", "data": { "t": 1000, "s": "BTC", "o": "1", "h": "1", "l": "1", "c": "1", "v": "1" } }).to_string());
        platform.advance(120_000.0);
        assert!(platform.warnings().is_empty());
        assert!(!ws.is_closed());
        assert_eq!(bars.borrow().len(), 1);
        unsub();
    }

    #[test]
    fn an_unexpected_close_reconnects_after_two_seconds_until_unsubscribed() {
        let (platform, provider) = setup();
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTC", "1h", sink, None).unwrap();
        platform.sockets()[0].emit_open();
        platform.sockets()[0].emit_close();
        platform.advance(1_999.0);
        assert_eq!(platform.sockets().len(), 1);
        platform.advance(1.0);
        assert_eq!(platform.sockets().len(), 2);
        platform.sockets()[1].emit_open();
        assert_eq!(platform.sockets()[1].sent().len(), 1); // it subscribes again
        platform.sockets()[1].emit_error();
        platform.advance(2_000.0);
        assert_eq!(platform.sockets().len(), 3);
        unsub();
        assert!(platform.sockets()[2].is_closed());
        platform.advance(60_000.0);
        assert_eq!(platform.sockets().len(), 3);
    }

    // ── HIP-3 markets ──

    fn main_meta() -> Value {
        json!({ "universe": [{ "name": "BTC", "szDecimals": 5, "maxLeverage": 40 }], "collateralToken": 0 })
    }

    fn hip3_spot_meta() -> Value {
        json!({
            "universe": [{ "tokens": [1, 0], "name": "PURR/USDC", "index": 0, "isCanonical": true }],
            // Token indexes are not array positions on the live API.
            "tokens": [{ "name": "USDC", "szDecimals": 8, "index": 0 }, { "name": "PURR", "szDecimals": 0, "index": 1 }, { "name": "USDH", "szDecimals": 2, "index": 360 }],
        })
    }

    fn dex_meta(name: &str) -> Value {
        match name {
            "xyz" => json!({ "universe": [
                { "name": "xyz:TSLA", "szDecimals": 3, "maxLeverage": 20 },
                { "name": "xyz:kGOLD", "szDecimals": 2, "maxLeverage": 10 },
                { "name": "xyz:OLD", "szDecimals": 2, "maxLeverage": 10, "isDelisted": true },
            ], "collateralToken": 0 }),
            "flx" => {
                json!({ "universe": [{ "name": "flx:SILVER", "szDecimals": 2, "maxLeverage": 10 }], "collateralToken": 360 })
            }
            "gone" => {
                json!({ "universe": [{ "name": "gone:NVDA", "szDecimals": 2, "maxLeverage": 10, "isDelisted": true }], "collateralToken": 0 })
            }
            _ => json!({ "__notok": true, "status": 500 }),
        }
    }

    fn hip3_route(b: &Value) -> Value {
        match b["type"].as_str() {
            Some("perpDexs") => {
                json!([null, { "name": "xyz", "fullName": "XYZ" }, { "name": "flx", "fullName": "Felix Exchange" }, { "name": "gone", "fullName": "Gone" }, { "name": "down", "fullName": "Down" }])
            }
            Some("spotMeta") => hip3_spot_meta(),
            Some("meta") if b.get("dex").is_none() => main_meta(),
            Some("meta") => dex_meta(b["dex"].as_str().unwrap_or("")),
            Some("candleSnapshot") => json!([candle(
                0,
                60,
                "1",
                "1",
                "1",
                "1",
                b["req"]["coin"].as_str().unwrap_or("")
            )]),
            _ => json!([]),
        }
    }

    #[test]
    fn search_lists_every_live_hip3_market_with_the_dex_that_lists_it() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let tsla = list.iter().find(|s| s.ticker == "xyz:TSLA").unwrap();
        assert_eq!(
            *tsla,
            SymbolDescriptor {
                ticker: "xyz:TSLA".into(),
                description: Some("TSLA / USD Perpetual (XYZ)".into()),
                kind: Some("futures".into()),
                ..SymbolDescriptor::default()
            }
        );
        let silver = list.iter().find(|s| s.ticker == "flx:SILVER").unwrap();
        assert_eq!(
            silver.description.as_deref(),
            Some("SILVER / USDH Perpetual (Felix Exchange)")
        );
    }

    #[test]
    fn search_leaves_out_delisted_markets_halted_dexes_and_dexes_that_fail_to_answer() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let tickers: Vec<&str> = list.iter().map(|s| s.ticker.as_str()).collect();
        assert!(!tickers.contains(&"xyz:OLD"));
        assert!(
            !tickers
                .iter()
                .any(|t| t.starts_with("gone:") || t.starts_with("down:"))
        );
        assert!(tickers.contains(&"xyz:TSLA"));
    }

    #[test]
    fn main_perps_and_spot_pairs_read_the_same_when_hip3_dexes_are_listed() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let btc = list.iter().find(|s| s.ticker == "BTC").unwrap();
        assert_eq!(
            *btc,
            SymbolDescriptor {
                ticker: "BTC".into(),
                description: Some("BTC / USD Perpetual".into()),
                kind: Some("futures".into()),
                ..SymbolDescriptor::default()
            }
        );
        let purr = list.iter().find(|s| s.ticker == "PURR/USDC").unwrap();
        assert_eq!(purr.description.as_deref(), Some("PURR / USDC"));
        assert_eq!(purr.kind.as_deref(), Some("crypto"));
        let info = platform
            .block_on(provider.get_symbol_info("BTC").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.extra["description"], json!("BTC / USD Perpetual"));
        assert_eq!(info.extra["currency"], json!("USD"));
        assert!((info.extra["mintick"].as_f64().unwrap() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn perps_and_spot_stay_searchable_when_the_dex_list_cannot_be_fetched() {
        let (platform, provider) = setup();
        stub_info(&platform, |b| {
            if b["type"] == "perpDexs" {
                json!({ "__notok": true, "status": 503 })
            } else {
                hip3_route(b)
            }
        });
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let tickers: Vec<&str> = list.iter().map(|s| s.ticker.as_str()).collect();
        assert_eq!(tickers, vec!["BTC", "PURR/USDC"]);
    }

    #[test]
    fn a_hip3_market_has_symbol_info_naming_its_coin_quote_and_dex() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let info = platform
            .block_on(provider.get_symbol_info("xyz:TSLA").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.ticker, "xyz:TSLA");
        assert_eq!(info.extra["tickerid"], json!("HYPERLIQUID:xyz:TSLA"));
        assert_eq!(
            info.extra["description"],
            json!("TSLA / USD Perpetual (XYZ)")
        );
        assert_eq!(info.extra["type"], json!("futures"));
        assert_eq!(info.extra["basecurrency"], json!("TSLA"));
        assert_eq!(info.extra["currency"], json!("USD"));
        assert_eq!(info.session.as_deref(), Some("24x7"));
        assert!((info.extra["mintick"].as_f64().unwrap() - 0.001).abs() < 1e-12); // 6 - szDecimals(3) = 3 decimals
        assert_eq!(info.extra["pricescale"], json!(1000));
    }

    #[test]
    fn a_hip3_market_margined_in_another_stablecoin_is_quoted_in_it() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let info = platform
            .block_on(provider.get_symbol_info("flx:SILVER").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.extra["currency"], json!("USDH"));
        assert_eq!(
            info.extra["description"],
            json!("SILVER / USDH Perpetual (Felix Exchange)")
        );
    }

    #[test]
    fn an_unknown_hip3_market_has_no_symbol_info() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        assert!(
            platform
                .block_on(provider.get_symbol_info("xyz:NOPE").unwrap())
                .unwrap()
                .is_none()
        );
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        assert!(
            platform
                .block_on(provider.get_symbol_info("down:TSLA").unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn loading_a_hip3_market_asks_hyperliquid_for_the_dex_coin() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        let bars = get_bars(&platform, &provider, "xyz:TSLA", "1h", limit(1));
        assert_eq!(bars.len(), 1);
        let snap = snapshot(&platform);
        assert_eq!(snap["req"]["coin"], "xyz:TSLA");
        assert_eq!(snap["req"]["interval"], "1h");
    }

    #[test]
    fn a_hip3_market_typed_in_any_case_loads_under_its_listed_spelling() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        get_bars(&platform, &provider, "XYZ:tsla", "1h", limit(1));
        // a second provider: the spelling is resolved per provider from the dex meta
        let other = HyperliquidProvider::new(platform.handle());
        get_bars(&platform, &other, "XYZ:KGOLD", "1h", limit(1));
        let coins: Vec<String> = calls(&platform)
            .iter()
            .filter(|c| c["type"] == "candleSnapshot")
            .map(|c| c["req"]["coin"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(coins, vec!["xyz:TSLA", "xyz:kGOLD"]);
    }

    #[test]
    fn a_known_spelling_is_reused_without_another_dex_request() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        get_bars(&platform, &provider, "XYZ:tsla", "1h", limit(1));
        get_bars(&platform, &provider, "xyz:KGOLD", "1h", limit(1));
        let kinds: Vec<String> = calls(&platform)
            .iter()
            .map(|c| c["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, vec!["meta", "candleSnapshot", "candleSnapshot"]);
        let coins: Vec<String> = calls(&platform)
            .iter()
            .filter(|c| c["type"] == "candleSnapshot")
            .map(|c| c["req"]["coin"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(coins, vec!["xyz:TSLA", "xyz:kGOLD"]);
    }

    #[test]
    fn a_failing_dex_request_leaves_the_typed_spelling_and_the_load_goes_on() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        get_bars(&platform, &provider, "down:tsla", "1h", limit(1));
        assert_eq!(snapshot(&platform)["req"]["coin"], "down:TSLA");
    }

    #[test]
    fn loading_a_main_perp_makes_no_hip3_request() {
        let (platform, provider) = setup();
        stub_info(&platform, hip3_route);
        get_bars(&platform, &provider, "BTC", "1h", limit(1));
        let kinds: Vec<String> = calls(&platform)
            .iter()
            .map(|c| c["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, vec!["candleSnapshot"]);
        assert_eq!(calls(&platform)[0]["req"]["coin"], "BTC");
    }

    #[test]
    fn a_hip3_market_streams_live_candles_under_its_dex_coin() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("xyz:TSLA", "1m", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        assert_eq!(
            ws.sent(),
            vec![
                "{\"method\":\"subscribe\",\"subscription\":{\"type\":\"candle\",\"coin\":\"xyz:TSLA\",\"interval\":\"1m\"}}"
            ]
        );
        // another coin: ignored
        ws.emit_message(&json!({ "channel": "candle", "data": { "t": 1000, "s": "BTC", "i": "1m", "o": "1", "h": "1", "l": "1", "c": "1", "v": "1", "n": 1 } }).to_string());
        ws.emit_message(&json!({ "channel": "candle", "data": { "t": 2000, "s": "xyz:TSLA", "i": "1m", "o": "358", "h": "359", "l": "357", "c": "358.5", "v": "4", "n": 3 } }).to_string());
        assert_eq!(
            *bars.borrow(),
            vec![bar(2000, 358.0, 359.0, 357.0, 358.5, 4.0)]
        );
        unsub();
    }

    #[test]
    fn a_hip3_markets_icon_is_looked_up_by_its_coin_not_its_dex() {
        let (_, provider) = setup();
        assert_eq!(
            provider
                .resolve_symbol_icon(&SymbolDescriptor::new("hyna:BTC"))
                .as_deref(),
            Some("https://crypto-icons.ledger.com/BTC.png")
        );
        let described = SymbolDescriptor {
            ticker: "hyna:BTC".into(),
            description: Some("BTC / USD Perpetual (HyENA)".into()),
            ..SymbolDescriptor::default()
        };
        assert_eq!(
            provider.resolve_symbol_icon(&described).as_deref(),
            Some("https://crypto-icons.ledger.com/BTC.png")
        );
    }

    #[test]
    fn the_provider_states_its_info_and_icons() {
        let (_, provider) = setup();
        let info = provider.info().unwrap();
        assert_eq!(info.name, "hyperliquid");
        assert_eq!(info.display_name.as_deref(), Some("Hyperliquid"));
        assert_eq!(info.requires_api_key, Some(false));
        assert_eq!(info.supported_timeframes.unwrap().len(), 16);
        let btc = SymbolDescriptor {
            ticker: "BTCUSDT".into(),
            description: Some("BTC / USDT".into()),
            ..SymbolDescriptor::default()
        };
        assert_eq!(
            provider.resolve_symbol_icon(&btc).as_deref(),
            Some("https://crypto-icons.ledger.com/BTC.png")
        );
        assert_eq!(
            provider
                .resolve_symbol_icon(&SymbolDescriptor::new("ETH-USD"))
                .as_deref(),
            Some("https://crypto-icons.ledger.com/ETH.png")
        );
    }

    #[test]
    fn concurrent_meta_reads_share_one_request() {
        let (platform, provider) = setup();
        stub_info(&platform, route);
        let a = provider.get_symbol_info("BTC").unwrap();
        let b = provider.get_symbol_info("ETH").unwrap();
        platform.block_on(async move {
            let _ = (a.await, b.await);
        });
        let metas = calls(&platform)
            .iter()
            .filter(|c| c["type"] == "meta")
            .count();
        assert_eq!(metas, 1);
    }
}
