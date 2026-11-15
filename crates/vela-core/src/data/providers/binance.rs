//! Binance market-data provider, built on the public REST and WebSocket APIs
//! (no third-party SDK, no API key). Serves spot and USDT-margined perpetual
//! futures (`SYMBOL.P`), pages past the 1000-kline cap, fails over
//! `api.binance.com` to `api.binance.us`, and aggregates timeframes Binance does
//! not serve natively (`45`, `180`). Live ticks stream from a native kline
//! WebSocket, with a poll fallback for aggregated timeframes and for a stream
//! that stays silent.
//!
//! Also here: [`RateGate`], the per-IP request gate of the Binance trade path
//! (concurrency cap, weight-driven pause, ban fail-fast). The provider itself
//! never routes a request through it; it is a standalone building block.
//!
//! The original's constructor takes no options. The WebSocket-availability check
//! of `subscribe` (`typeof WebSocket !== 'undefined'`) is gone: a [`Platform`]
//! always offers sockets.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::rc::Rc;

use serde_json::Value;

use super::common::{
    FetchError, Memo, SharedSink, SymbolFields, build_symbol_info, encode_query_value, is_present,
    join_all, js_num, js_parse_float, js_text, now_ms, parse_json, parse_minutes, poll_bars,
    select_sub_tf, tail,
};
use crate::data::{base_of, ledger_crypto_icon_url};
use crate::model::Ohlcv;
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::{BarRange, BarSink, SymbolInfo};
use crate::runtime::{
    self, DataError, HttpRequest, LocalBoxFuture, OneshotSender, Platform, Socket, SocketHandlers,
    TimerId, Unsubscribe, oneshot,
};

pub use super::common::{aggregate, dedupe_sorted, normalize_tf};

const SPOT_BASE: &str = "https://api.binance.com/api/v3";
const SPOT_BASE_US: &str = "https://api.binance.us/api/v3";
const FUTURES_BASE: &str = "https://fapi.binance.com/fapi/v1";
const SPOT_WS: &str = "wss://stream.binance.com:9443";
const SPOT_WS_US: &str = "wss://stream.binance.us:9443";
const FUTURES_WS: &str = "wss://fstream.binance.com";
/// If a kline socket opens but delivers no candle within this window, treat it as
/// non-delivering and fall back to polling. Covers a socket that connects but stays
/// silent, e.g. Binance USD-M futures streams are geo-restricted in some regions and
/// ACK the subscription yet never push data. Generous enough not to mis-fire on an
/// illiquid symbol (which degrades to polling harmlessly anyway).
const STREAM_STALL_MS: f64 = 15_000.0;
/// Reconnect backoff after an unexpected socket close.
const STREAM_RECONNECT_MS: f64 = 2_000.0;

/// Native intraday timeframes in minutes (the aggregation sub-candle candidates).
const NATIVE_MINUTES: [usize; 11] = [1, 3, 5, 15, 30, 60, 120, 240, 360, 480, 720];
const SUPPORTED_TIMEFRAMES: [&str; 13] = [
    "1", "3", "5", "15", "30", "45", "60", "120", "180", "240", "D", "W", "M",
];

/// One raw Binance kline row: `[openTime, open, high, low, close, volume, closeTime, …]`.
pub type RawKline = Vec<Value>;

/// Canonical timeframe to Binance interval string (native, no aggregation).
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
        "360" => "6h",
        "480" => "8h",
        "720" => "12h",
        "D" => "1d",
        "W" => "1w",
        "M" => "1M",
        _ => return None,
    })
}

/// A Vela ticker split into the Binance API symbol and whether it is a perpetual (`.P`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTicker {
    pub api_symbol: String,
    pub is_futures: bool,
}

pub fn parse_ticker(ticker: &str) -> ParsedTicker {
    let t = ticker.trim().to_uppercase();
    match t.strip_suffix(".P") {
        Some(symbol) => ParsedTicker {
            api_symbol: symbol.to_string(),
            is_futures: true,
        },
        None => ParsedTicker {
            api_symbol: t,
            is_futures: false,
        },
    }
}

/// Map raw Binance klines to neutral bars (open-time in epoch ms).
pub fn klines_to_ohlcv(raw: &[RawKline]) -> Vec<Ohlcv> {
    raw.iter()
        .map(|k| {
            Ohlcv::new(
                js_num(k.first()) as i64,
                js_num(k.get(1)),
                js_num(k.get(2)),
                js_num(k.get(3)),
                js_num(k.get(4)),
                Some(js_num(k.get(5))),
            )
        })
        .collect()
}

/// Map the kline payload (`k`) of a Binance `@kline` WebSocket event to a neutral bar.
pub fn kline_event_to_ohlcv(k: &Value) -> Ohlcv {
    Ohlcv::new(
        js_num(k.get("t")) as i64,
        js_num(k.get("o")),
        js_num(k.get("h")),
        js_num(k.get("l")),
        js_num(k.get("c")),
        Some(js_num(k.get("v"))),
    )
}

fn symbols_of(data: &Value) -> &[Value] {
    data.get("symbols")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

struct Inner {
    platform: Rc<dyn Platform>,
    /// The spot endpoint confirmed reachable (cached after the first probe).
    spot_base_url: RefCell<Option<String>>,
    /// Waiters of the in-flight endpoint probe, shared so concurrent first calls
    /// don't each ping. `None` while no probe runs.
    probe: RefCell<Option<Vec<OneshotSender<String>>>>,
    /// Cached symbol enumeration (exchangeInfo is large; fetch once).
    symbols: Memo<Vec<SymbolDescriptor>>,
}

/// The Binance provider: a cheap-to-clone handle.
#[derive(Clone)]
pub struct BinanceProvider {
    inner: Rc<Inner>,
}

impl BinanceProvider {
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                spot_base_url: RefCell::new(None),
                probe: RefCell::new(None),
                symbols: Memo::default(),
            }),
        }
    }
}

impl DataProvider for BinanceProvider {
    fn supports_stream(&self) -> bool {
        true
    }

    fn supports_symbol_info(&self) -> bool {
        true
    }

    fn info(&self) -> Option<ProviderInfo> {
        Some(ProviderInfo {
            name: "binance".to_string(),
            display_name: Some("Binance".to_string()),
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
                let spot = inner.clone();
                let futures = inner.clone();
                let lists: Vec<LocalBoxFuture<'static, Vec<SymbolDescriptor>>> = vec![
                    Box::pin(async move { spot.list_spot().await.unwrap_or_default() }),
                    Box::pin(async move { futures.list_futures().await.unwrap_or_default() }),
                ];
                join_all(lists).await.into_iter().flatten().collect()
            })
        });
        Some(Box::pin(async move { Ok(shared.await) }))
    }

    /// Predefined icon source for a crypto venue: the Ledger crypto-icon CDN, keyed by
    /// the BASE asset (the description's first segment, else the de-suffixed ticker).
    fn resolve_symbol_icon(&self, symbol: &SymbolDescriptor) -> Option<String> {
        ledger_crypto_icon_url(&base_of(symbol))
    }

    fn subscribe(
        &self,
        ticker: &str,
        timeframe: &str,
        on_bar: BarSink,
        _session: Option<&str>,
    ) -> Option<Unsubscribe> {
        let inner = self.inner.clone();
        let ParsedTicker {
            api_symbol,
            is_futures,
        } = parse_ticker(ticker);
        let on_bar = SharedSink::new(on_bar);
        // A native interval streams over the WebSocket; an aggregated timeframe polls getBars.
        Some(match interval_of(&normalize_tf(timeframe)) {
            Some(interval) => StreamCtx::start(
                inner,
                ticker,
                timeframe,
                format!("{}@kline_{interval}", api_symbol.to_lowercase()),
                is_futures,
                on_bar,
            ),
            None => inner.poll(ticker, timeframe, on_bar),
        })
    }
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
                        "[vela] Binance: failed to fetch {ticker} {timeframe} — {e}"
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
        let ParsedTicker {
            api_symbol,
            is_futures,
        } = parse_ticker(ticker);
        let base = if is_futures {
            FUTURES_BASE.to_string()
        } else {
            self.spot_base().await
        };
        let tf = normalize_tf(timeframe);

        if let Some(interval) = interval_of(&tf) {
            let raw = self
                .fetch_klines(&base, &api_symbol, interval, range)
                .await?;
            return Ok(dedupe_sorted(&klines_to_ohlcv(&raw)));
        }

        // Aggregation path: fetch a native sub-timeframe and combine.
        let target_min = parse_minutes(&tf);
        let sub_min = target_min.and_then(|t| select_sub_tf(&NATIVE_MINUTES, t));
        let (Some(target_min), Some(sub_min)) = (target_min, sub_min) else {
            self.platform.warn(&format!(
                "[vela] Binance: timeframe \"{timeframe}\" is not supported and cannot be aggregated."
            ));
            return Ok(Vec::new());
        };
        let ratio = target_min / sub_min;
        let sub_range = BarRange {
            limit: range.limit.map(|l| l * ratio + ratio),
            ..range.clone()
        };
        let interval = interval_of(&sub_min.to_string()).unwrap_or("1m");
        let sub = klines_to_ohlcv(
            &self
                .fetch_klines(&base, &api_symbol, interval, &sub_range)
                .await?,
        );
        let agg = aggregate(&sub, target_min as i64 * 60_000);
        Ok(tail(agg, range.limit))
    }

    async fn symbol_info(self: &Rc<Self>, ticker: &str) -> Option<SymbolInfo> {
        let ParsedTicker {
            api_symbol,
            is_futures,
        } = parse_ticker(ticker);
        let base = if is_futures {
            FUTURES_BASE.to_string()
        } else {
            self.spot_base().await
        };
        // Spot accepts ?symbol=; the futures exchangeInfo returns all symbols.
        let url = if is_futures {
            format!("{base}/exchangeInfo")
        } else {
            format!("{base}/exchangeInfo?symbol={api_symbol}")
        };
        let data = self.json(&url).await.unwrap_or(Value::Null);
        let symbols = symbols_of(&data);
        let s = if is_futures {
            symbols
                .iter()
                .find(|x| x.get("symbol").and_then(Value::as_str) == Some(api_symbol.as_str()))
        } else {
            symbols.first()
        }?;

        let tick_size = s
            .get("filters")
            .and_then(Value::as_array)
            .and_then(|filters| {
                filters
                    .iter()
                    .find(|f| f.get("filterType").and_then(Value::as_str) == Some("PRICE_FILTER"))
            })
            .map_or(0.01, |filter| match filter.get("tickSize") {
                Some(v) if is_present(Some(v)) => js_parse_float(&js_text(Some(v))),
                _ => js_parse_float("0.01"),
            });
        let base_asset = s.get("baseAsset");
        let quote_asset = s.get("quoteAsset");
        Some(build_symbol_info(SymbolFields {
            venue: "BINANCE",
            ticker, // keep the original, incl. any .P (as Pine Script expects)
            description: format!(
                "{} / {}{}",
                js_text(base_asset),
                js_text(quote_asset),
                if is_futures { " Perpetual" } else { "" }
            ),
            kind: if is_futures { "futures" } else { "crypto" },
            base: base_asset.cloned(),
            quote: quote_asset.cloned(),
            mintick: tick_size,
        }))
    }

    async fn list_spot(self: &Rc<Self>) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let base = self.spot_base().await;
        let data = self.json(&format!("{base}/exchangeInfo")).await?;
        Ok(symbols_of(&data)
            .iter()
            .filter(|s| s.get("status").and_then(Value::as_str) == Some("TRADING"))
            .map(|s| SymbolDescriptor {
                ticker: js_text(s.get("symbol")),
                description: Some(format!(
                    "{} / {}",
                    js_text(s.get("baseAsset")),
                    js_text(s.get("quoteAsset"))
                )),
                kind: Some("crypto".to_string()),
                ..SymbolDescriptor::default()
            })
            .collect())
    }

    async fn list_futures(self: &Rc<Self>) -> Result<Vec<SymbolDescriptor>, FetchError> {
        let data = self.json(&format!("{FUTURES_BASE}/exchangeInfo")).await?;
        Ok(symbols_of(&data)
            .iter()
            .filter(|s| {
                s.get("contractType").and_then(Value::as_str) == Some("PERPETUAL")
                    && s.get("status").and_then(Value::as_str) == Some("TRADING")
            })
            .map(|s| SymbolDescriptor {
                ticker: format!("{}.P", js_text(s.get("symbol"))),
                description: Some(format!(
                    "{} / {} Perpetual",
                    js_text(s.get("baseAsset")),
                    js_text(s.get("quoteAsset"))
                )),
                kind: Some("futures".to_string()),
                ..SymbolDescriptor::default()
            })
            .collect())
    }

    /// Poll the forming candle (for aggregated timeframes Binance has no native kline stream for).
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

    /// WebSocket host matching the resolved REST spot endpoint (`.com` default, else `.us`).
    async fn spot_ws_base(self: &Rc<Self>) -> &'static str {
        if self.spot_base().await.contains("binance.us") {
            SPOT_WS_US
        } else {
            SPOT_WS
        }
    }

    /// Fetch klines, paginating past Binance's 1000-row cap.
    async fn fetch_klines(
        &self,
        base: &str,
        symbol: &str,
        interval: &str,
        range: &BarRange,
    ) -> Result<Vec<RawKline>, FetchError> {
        if let Some(from) = range.from {
            let to = range.to.unwrap_or_else(|| now_ms(&self.platform));
            return self
                .paginate_forward(base, symbol, interval, from, to, range.limit)
                .await;
        }
        let limit = range.limit.unwrap_or(500);
        if limit > 1000 {
            return self
                .paginate_backward(base, symbol, interval, limit, range.to)
                .await;
        }
        self.klines_chunk(
            base,
            Chunk {
                symbol,
                interval,
                limit: Some(limit),
                start: None,
                end: range.to.map(|t| t as f64),
            },
        )
        .await
    }

    /// Forward pagination from `from` to `to` (used by ranged/tail fetches).
    async fn paginate_forward(
        &self,
        base: &str,
        symbol: &str,
        interval: &str,
        from: i64,
        to: i64,
        limit: Option<usize>,
    ) -> Result<Vec<RawKline>, FetchError> {
        let limit = limit.filter(|l| *l > 0);
        let mut out: Vec<RawKline> = Vec::new();
        let mut cursor = from as f64;
        let to = to as f64;
        while cursor < to {
            let chunk = self
                .klines_chunk(
                    base,
                    Chunk {
                        symbol,
                        interval,
                        limit: Some(1000),
                        start: Some(cursor),
                        end: Some(to),
                    },
                )
                .await?;
            let Some(last) = chunk.last() else { break };
            cursor = js_num(last.get(6)) + 1.0; // last closeTime + 1ms
            let full = chunk.len() >= 1000;
            out.extend(chunk);
            if !full {
                break;
            }
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
    }

    /// Backward pagination to assemble the most-recent `limit` bars.
    async fn paginate_backward(
        &self,
        base: &str,
        symbol: &str,
        interval: &str,
        limit: usize,
        end_time: Option<i64>,
    ) -> Result<Vec<RawKline>, FetchError> {
        let mut out: Vec<RawKline> = Vec::new();
        let mut remaining = limit;
        let mut cursor = end_time.map(|t| t as f64);
        let mut guard = limit.div_ceil(1000) + 5;
        while remaining > 0 && guard > 0 {
            guard -= 1;
            let size = remaining.min(1000);
            let mut chunk = self
                .klines_chunk(
                    base,
                    Chunk {
                        symbol,
                        interval,
                        limit: Some(size),
                        start: None,
                        end: cursor,
                    },
                )
                .await?;
            if chunk.is_empty() {
                break;
            }
            let count = chunk.len();
            cursor = Some(js_num(chunk[0].first()) - 1.0); // earliest openTime - 1ms
            chunk.append(&mut out);
            out = chunk;
            remaining = remaining.saturating_sub(count);
            if count < size {
                break;
            }
        }
        Ok(out)
    }

    async fn klines_chunk(
        &self,
        base: &str,
        params: Chunk<'_>,
    ) -> Result<Vec<RawKline>, FetchError> {
        let mut url = format!(
            "{base}/klines?symbol={}&interval={}",
            encode_query_value(params.symbol),
            encode_query_value(params.interval)
        );
        if let Some(limit) = params.limit.filter(|l| *l > 0) {
            url.push_str(&format!("&limit={}", limit.min(1000)));
        }
        if let Some(start) = params.start {
            url.push_str(&format!("&startTime={}", crate::util::js_number(start)));
        }
        if let Some(end) = params.end {
            url.push_str(&format!("&endTime={}", crate::util::js_number(end)));
        }
        let data = self.json(&url).await?;
        let Some(rows) = data.as_array() else {
            return Ok(Vec::new());
        };
        // Binance returns an array OF arrays; guard against a malformed flat array.
        if rows.first().is_some_and(|row| !row.is_array()) {
            return Ok(Vec::new());
        }
        Ok(rows
            .iter()
            .map(|row| row.as_array().cloned().unwrap_or_default())
            .collect())
    }

    /// Resolve the reachable spot endpoint (default, else the US mirror). The in-flight
    /// probe is shared, so a burst of concurrent first calls issues ONE ping pair, not one
    /// per call. A confirmed endpoint is cached permanently; a total failure clears the
    /// probe so a later call can retry.
    fn spot_base(self: &Rc<Self>) -> LocalBoxFuture<'static, String> {
        let cached = self.spot_base_url.borrow().clone();
        if let Some(url) = cached {
            return Box::pin(async move { url });
        }
        let (tx, rx) = oneshot();
        let start = {
            let mut probe = self.probe.borrow_mut();
            match probe.as_mut() {
                Some(waiters) => {
                    waiters.push(tx);
                    false
                }
                None => {
                    *probe = Some(vec![tx]);
                    true
                }
            }
        };
        if start {
            let inner = self.clone();
            self.platform.spawn(Box::pin(async move {
                let url = inner.probe_endpoints().await;
                let waiters = inner.probe.borrow_mut().take().unwrap_or_default();
                for waiter in waiters {
                    waiter.send(url.clone());
                }
            }));
        }
        Box::pin(rx)
    }

    async fn probe_endpoints(&self) -> String {
        for url in [SPOT_BASE, SPOT_BASE_US] {
            let ping = HttpRequest::get(format!("{url}/ping")).timeout(5000.0);
            // A failed or non-2xx ping: try the next endpoint.
            if let Ok(res) = self.platform.fetch(ping).await
                && res.ok()
            {
                *self.spot_base_url.borrow_mut() = Some(url.to_string());
                return url.to_string();
            }
        }
        SPOT_BASE.to_string() // give up; let the actual request surface the error
    }

    async fn json(&self, url: &str) -> Result<Value, FetchError> {
        let res = self.platform.fetch(HttpRequest::get(url)).await?;
        if !res.ok() {
            return Err(FetchError::new(format!(
                "Binance HTTP {} for {url}",
                res.status
            )));
        }
        parse_json(&res)
    }
}

struct Chunk<'a> {
    symbol: &'a str,
    interval: &'a str,
    limit: Option<usize>,
    start: Option<f64>,
    end: Option<f64>,
}

/// One live kline subscription. A socket opened on the single-stream URL form (the
/// stream is in the path, so no subscribe frame is sent): perpetuals use `fstream`,
/// spot uses the host matching the resolved REST endpoint. It reconnects on an
/// unexpected close until unsubscribed; the platform answers Binance's
/// protocol-level pings, so no manual keepalive is needed.
///
/// A stall watchdog falls the subscription back to polling if the socket opens but
/// never delivers a candle, so a blocked or silent stream (e.g. geo-restricted
/// futures) still yields live updates instead of a silently dead feed.
struct StreamCtx {
    provider: Rc<Inner>,
    ticker: String,
    timeframe: String,
    stream: String,
    is_futures: bool,
    on_bar: SharedSink,
    closed: Cell<bool>,
    ws: RefCell<Option<Rc<dyn Socket>>>,
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
        stream: String,
        is_futures: bool,
        on_bar: SharedSink,
    ) -> Unsubscribe {
        let ctx = Rc::new(StreamCtx {
            provider,
            ticker: ticker.to_string(),
            timeframe: timeframe.to_string(),
            stream,
            is_futures,
            on_bar,
            closed: Cell::new(false),
            ws: RefCell::new(None),
            reconnect: Cell::new(None),
            stall: Cell::new(None),
            polling: Cell::new(false),
            stop_polling: RefCell::new(None),
        });
        ctx.open();
        Box::new(move || {
            ctx.closed.set(true);
            ctx.clear_stall();
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

    /// Tear down the socket and switch to polling: covers an open-but-silent stream.
    fn fall_to_polling(self: &Rc<Self>) {
        if self.closed.get() || self.polling.get() {
            return;
        }
        self.clear_stall();
        if let Some(id) = self.reconnect.take() {
            self.provider.platform.clear_timeout(id);
        }
        let ws = self.ws.borrow_mut().take();
        self.provider.platform.warn(&format!(
            "[vela] Binance: {} {} stream delivered no data; falling back to polling.",
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

    /// Open (or reopen) the socket. Spot first resolves the host from the probed REST
    /// endpoint; the perpetual host is fixed, so its socket opens at once.
    fn open(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        if self.is_futures {
            self.connect(FUTURES_WS);
            return;
        }
        let ctx = self.clone();
        self.provider.platform.spawn(Box::pin(async move {
            let base = ctx.provider.spot_ws_base().await;
            if ctx.closed.get() {
                return; // unsubscribed during the host probe
            }
            ctx.connect(base);
        }));
    }

    fn connect(self: &Rc<Self>, base: &str) {
        let platform = &self.provider.platform;
        let on_message = {
            let ctx = self.clone();
            move |text: &str| {
                if ctx.closed.get() || ctx.polling.get() {
                    return; // unsubscribed, or already fell back to polling
                }
                // Non-JSON and control frames are ignored.
                let Ok(msg) = serde_json::from_str::<Value>(text) else {
                    return;
                };
                if let Some(k) = msg.get("k").filter(|k| k.is_object()) {
                    ctx.clear_stall();
                    ctx.on_bar.emit(kline_event_to_ohlcv(k));
                }
            }
        };
        let on_close = {
            let ctx = self.clone();
            move || {
                if !ctx.closed.get() && !ctx.polling.get() {
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
            &format!("{base}/ws/{}", self.stream),
            SocketHandlers {
                on_open: None,
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

/// Thrown by [`RateGate::run`] when a request is rate-limited (429) or the IP is
/// banned (418, `banned = true`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitError {
    pub message: String,
    pub banned: bool,
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RateLimitError {}

impl From<RateLimitError> for DataError {
    fn from(e: RateLimitError) -> Self {
        DataError::Other(e.message)
    }
}

/// Pause to the next window once a response reports at least this fraction of the weight cap.
pub const DEFAULT_THROTTLE_FRACTION: f64 = 0.8;

struct GateState {
    platform: Rc<dyn Platform>,
    max_concurrent: usize,
    throttle_fraction: f64,
    active: Cell<usize>,
    waiters: RefCell<VecDeque<OneshotSender<()>>>,
    paused_until: Cell<f64>,
    banned_until: Cell<f64>,
}

/// A per-IP request gate for the Binance trade path. It does three things, all aimed
/// at never tripping a ban:
///
///  - Concurrency cap: at most `max_concurrent` requests in flight, so overlapping
///    burst-prone backfills (initial + scroll + live poll) cannot fan out at once.
///  - Pause: set from a 429 `Retry-After`/backoff ([`pause_for`](Self::pause_for)) or
///    pre-emptively when a response's `X-MBX-USED-WEIGHT-1M` nears the cap
///    ([`note_weight`](Self::note_weight)); new work waits it out before acquiring a slot.
///  - Ban: a 418 marks the IP banned ([`ban_for`](Self::ban_for)); subsequent runs fail
///    fast with a [`RateLimitError`] instead of hammering the API (which only extends the ban).
///
/// One instance is shared across every call of a provider, so it is a cheap-to-clone
/// handle. It does no HTTP and no logging, and reads its clock and timers from the
/// platform. A slot is released when the task finishes or its future is dropped.
#[derive(Clone)]
pub struct RateGate {
    state: Rc<GateState>,
}

/// Returns the slot when the running task ends, even if its future is dropped.
struct Slot(Rc<GateState>);

impl Drop for Slot {
    fn drop(&mut self) {
        let state = &self.0;
        state.active.set(state.active.get().saturating_sub(1));
        let next = state.waiters.borrow_mut().pop_front();
        if let Some(waiter) = next {
            state.active.set(state.active.get() + 1);
            waiter.send(());
        }
    }
}

impl RateGate {
    pub fn new(platform: Rc<dyn Platform>, max_concurrent: usize) -> Self {
        Self::with_throttle_fraction(platform, max_concurrent, DEFAULT_THROTTLE_FRACTION)
    }

    pub fn with_throttle_fraction(
        platform: Rc<dyn Platform>,
        max_concurrent: usize,
        throttle_fraction: f64,
    ) -> Self {
        Self {
            state: Rc::new(GateState {
                platform,
                max_concurrent,
                throttle_fraction,
                active: Cell::new(0),
                waiters: RefCell::new(VecDeque::new()),
                paused_until: Cell::new(0.0),
                banned_until: Cell::new(0.0),
            }),
        }
    }

    /// Acquire a slot (after any pause), run `f`, release. Fails with a
    /// [`RateLimitError`] while banned.
    pub async fn run<T, F: Future<Output = T>>(
        &self,
        f: impl FnOnce() -> F,
    ) -> Result<T, RateLimitError> {
        self.assert_not_banned()?;
        self.wait_for_pause().await?;
        self.acquire().await;
        let _slot = Slot(self.state.clone());
        Ok(f().await)
    }

    /// Note consumed request weight from a response; pause to the next minute window
    /// if near the cap.
    pub fn note_weight(&self, used: f64, limit: f64) {
        if limit > 0.0 && used >= limit * self.state.throttle_fraction {
            let now = self.state.platform.now_ms();
            let next_window = ((now + 1.0) / 60_000.0).ceil() * 60_000.0; // the weight counter resets each minute
            self.state
                .paused_until
                .set(self.state.paused_until.get().max(next_window));
        }
    }

    /// Pause new requests for `ms` (a 429 Retry-After / exponential backoff).
    pub fn pause_for(&self, ms: f64) {
        if ms > 0.0 {
            let until = self.state.platform.now_ms() + ms;
            self.state
                .paused_until
                .set(self.state.paused_until.get().max(until));
        }
    }

    /// Mark the IP banned for `ms` (a 418) so further requests fail fast.
    pub fn ban_for(&self, ms: f64) {
        if ms > 0.0 {
            let until = self.state.platform.now_ms() + ms;
            self.state
                .banned_until
                .set(self.state.banned_until.get().max(until));
        }
    }

    /// Remaining ban time in ms (0 if not banned), for diagnostics.
    pub fn ban_remaining_ms(&self) -> f64 {
        (self.state.banned_until.get() - self.state.platform.now_ms()).max(0.0)
    }

    fn assert_not_banned(&self) -> Result<(), RateLimitError> {
        if self.state.platform.now_ms() < self.state.banned_until.get() {
            return Err(RateLimitError {
                message: format!(
                    "Binance IP banned for ~{}s",
                    crate::util::js_number((self.ban_remaining_ms() / 1000.0).ceil())
                ),
                banned: true,
            });
        }
        Ok(())
    }

    async fn wait_for_pause(&self) -> Result<(), RateLimitError> {
        let state = &self.state;
        let mut wait = state.paused_until.get() - state.platform.now_ms();
        while wait > 0.0 {
            runtime::sleep(&*state.platform, wait).await;
            // pausedUntil may have been extended while we slept
            wait = state.paused_until.get() - state.platform.now_ms();
        }
        self.assert_not_banned()
    }

    async fn acquire(&self) {
        let state = &self.state;
        if state.active.get() < state.max_concurrent {
            state.active.set(state.active.get() + 1);
            return;
        }
        let (tx, rx) = oneshot();
        state.waiters.borrow_mut().push_back(tx);
        rx.await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use crate::runtime::{HttpResponse, NetError};

    const MIN: i64 = 60_000;

    fn setup() -> (ManualPlatform, BinanceProvider) {
        let platform = ManualPlatform::new(1_000_000.0);
        let provider = BinanceProvider::new(platform.handle());
        (platform, provider)
    }

    fn response(status: u16, body: Value) -> Option<Result<HttpResponse, NetError>> {
        Some(Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }))
    }

    fn kline(open_time: i64, close_time: i64) -> Value {
        json!([open_time, "10", "11", "9", "10", "5", close_time])
    }

    /// `/ping` answers 200; `/klines` answers `rows`; anything else is a 404.
    fn serve_klines(platform: &ManualPlatform, rows: Value) {
        platform.on_fetch(move |req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else if req.url.contains("/klines") {
                response(200, rows.clone())
            } else {
                response(404, json!({}))
            }
        });
    }

    fn urls(platform: &ManualPlatform) -> Vec<String> {
        platform.requests().into_iter().map(|r| r.url).collect()
    }

    fn get_bars(
        platform: &ManualPlatform,
        provider: &BinanceProvider,
        ticker: &str,
        tf: &str,
        range: BarRange,
    ) -> Vec<Ohlcv> {
        platform
            .block_on(provider.get_bars(ticker, tf, &range))
            .unwrap()
    }

    fn limit(n: usize) -> BarRange {
        BarRange {
            limit: Some(n),
            ..BarRange::default()
        }
    }

    fn bar(time: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Ohlcv {
        Ohlcv::new(time, o, h, l, c, Some(v))
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
    fn parse_ticker_splits_perpetual_futures_from_spot() {
        let spot = ParsedTicker {
            api_symbol: "BTCUSDT".into(),
            is_futures: false,
        };
        let perp = ParsedTicker {
            api_symbol: "BTCUSDT".into(),
            is_futures: true,
        };
        assert_eq!(parse_ticker("BTCUSDT"), spot);
        assert_eq!(parse_ticker("BTCUSDT.P"), perp);
        assert_eq!(parse_ticker("btcusdt.p"), perp); // upper-cased
    }

    #[test]
    fn klines_map_raw_rows_with_open_time_in_ms() {
        let rows = vec![vec![
            json!(1000),
            json!("10"),
            json!("12"),
            json!("9"),
            json!("11"),
            json!("100"),
            json!(4999),
        ]];
        assert_eq!(
            klines_to_ohlcv(&rows),
            vec![bar(1000, 10.0, 12.0, 9.0, 11.0, 100.0)]
        );
    }

    #[test]
    fn kline_event_maps_a_ws_payload_with_string_prices() {
        let k =
            json!({ "t": 1000, "o": "10", "h": "12", "l": "9", "c": "11", "v": "100", "x": false });
        assert_eq!(
            kline_event_to_ohlcv(&k),
            bar(1000, 10.0, 12.0, 9.0, 11.0, 100.0)
        );
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
        // six 15-min candles make two 45-min candles ([0,45) and [45,90))
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
            out.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![0, 45 * MIN]
        );
        // first bucket: open from candle@0, close from candle@30, high/low across the three
        assert_eq!(out[0], bar(0, 100.0, 112.0, 90.0, 103.0, 3.0));
        assert_eq!(out[1], bar(45 * MIN, 103.0, 115.0, 93.0, 106.0, 3.0));
    }

    // ── getBars ──

    #[test]
    fn a_native_timeframe_is_fetched_directly_and_mapped() {
        let (platform, provider) = setup();
        let rows: Vec<Value> = (0..3)
            .map(|i| kline(i * 3_600_000, i * 3_600_000 + 3_599_999))
            .collect();
        serve_klines(&platform, Value::Array(rows));

        let bars = get_bars(&platform, &provider, "BTCUSDT", "1h", limit(3));
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0], bar(0, 10.0, 11.0, 9.0, 10.0, 5.0));
        let urls = urls(&platform);
        assert!(
            urls.iter()
                .any(|u| u.contains("interval=1h") && u.contains("symbol=BTCUSDT"))
        );
        assert!(
            urls.iter()
                .any(|u| u.ends_with("/klines?symbol=BTCUSDT&interval=1h&limit=3"))
        );
    }

    #[test]
    fn an_unsupported_timeframe_aggregates_a_native_sub_timeframe() {
        // 6 fifteen-minute candles: the provider requests interval=15m and returns 45m bars
        let (platform, provider) = setup();
        let rows: Vec<Value> = [0, 15, 30, 45, 60, 75]
            .iter()
            .map(|m| {
                json!([
                    m * MIN,
                    "100",
                    "110",
                    "90",
                    "101",
                    "1",
                    m * MIN + 15 * MIN - 1
                ])
            })
            .collect();
        serve_klines(&platform, Value::Array(rows));

        let bars = get_bars(&platform, &provider, "BTCUSDT", "45", limit(2));
        assert!(urls(&platform).iter().any(|u| u.contains("interval=15m")));
        // limit 2 at ratio 3 asks for 2 * 3 + 3 sub-candles
        assert!(urls(&platform).iter().any(|u| u.contains("limit=9")));
        assert_eq!(
            bars.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![0, 45 * MIN]
        );
    }

    #[test]
    fn a_timeframe_that_cannot_be_aggregated_warns_and_returns_nothing() {
        let (platform, provider) = setup();
        serve_klines(&platform, json!([]));
        assert!(get_bars(&platform, &provider, "BTCUSDT", "x", limit(3)).is_empty());
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Binance: timeframe \"x\" is not supported and cannot be aggregated."]
        );
    }

    #[test]
    fn a_dot_p_ticker_goes_to_the_futures_endpoint() {
        let (platform, provider) = setup();
        serve_klines(&platform, json!([kline(0, 3_599_999)]));
        get_bars(&platform, &provider, "BTCUSDT.P", "1h", limit(1));
        let urls = urls(&platform);
        assert!(
            urls.iter()
                .any(|u| u.contains("fapi.binance.com") && u.contains("symbol=BTCUSDT"))
        );
        assert!(!urls.iter().any(|u| u.contains("symbol=BTCUSDT.P"))); // .P stripped for the API
        assert!(!urls.iter().any(|u| u.contains("/ping"))); // no spot probe for a perpetual
    }

    #[test]
    fn a_failed_fetch_fails_soft_with_a_warning() {
        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                response(503, json!({})) // klines 503
            }
        });
        assert!(get_bars(&platform, &provider, "BTCUSDT", "1h", limit(5)).is_empty());
        let warnings = platform.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("failed to fetch"));
        assert!(warnings[0].contains("Binance HTTP 503"));
    }

    #[test]
    fn a_malformed_body_or_a_network_error_fails_soft() {
        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                Some(Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: "<html>not json".to_string(),
                }))
            }
        });
        assert!(get_bars(&platform, &provider, "BTCUSDT", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].contains("failed to fetch BTCUSDT 1h"));

        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                Some(Err(NetError::Failed("offline".to_string())))
            }
        });
        assert!(get_bars(&platform, &provider, "BTCUSDT", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].ends_with("— offline"));
    }

    #[test]
    fn a_flat_or_non_array_klines_answer_reads_as_empty() {
        for body in [json!([1, 2, 3]), json!({ "code": -1121 })] {
            let (platform, provider) = setup();
            serve_klines(&platform, body);
            assert!(get_bars(&platform, &provider, "BTCUSDT", "1h", limit(5)).is_empty());
            assert!(platform.warnings().is_empty());
        }
    }

    #[test]
    fn pagination_walks_backward_past_the_1000_kline_cap() {
        let (platform, provider) = setup();
        let step = 3_600_000;
        let kl = |t: i64| kline(t * step, t * step + step - 1);
        let page1: Vec<Value> = (0..1000).map(|i| kl(1000 + i)).collect(); // most-recent page
        let page2: Vec<Value> = (0..500).map(|i| kl(500 + i)).collect(); // older page
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |req| {
            if req.url.contains("/ping") {
                return response(200, json!({}));
            }
            counter.set(counter.get() + 1);
            match counter.get() {
                1 => response(200, Value::Array(page1.clone())),
                2 => response(200, Value::Array(page2.clone())),
                _ => response(200, json!([])),
            }
        });

        let bars = get_bars(&platform, &provider, "BTCUSDT", "1h", limit(1500));
        assert_eq!(calls.get(), 2); // 1000 + 500
        assert_eq!(bars.len(), 1500);
        assert!(bars[0].time < bars[bars.len() - 1].time); // merged + sorted ascending
        // the second page ends one ms before the first page's earliest open time
        let urls = urls(&platform);
        let klines: Vec<&String> = urls.iter().filter(|u| u.contains("/klines")).collect();
        assert!(klines[0].contains("limit=1000") && !klines[0].contains("endTime"));
        assert!(klines[1].contains("limit=500"));
        assert!(klines[1].ends_with(&format!("endTime={}", 1000 * step - 1)));
    }

    #[test]
    fn a_ranged_fetch_pages_forward_from_the_last_close_time() {
        let (platform, provider) = setup();
        let step = 60_000;
        let full: Vec<Value> = (0..1000)
            .map(|i| kline(i * step, i * step + step - 1))
            .collect();
        let tail: Vec<Value> = (1000..1010)
            .map(|i| kline(i * step, i * step + step - 1))
            .collect();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |req| {
            if req.url.contains("/ping") {
                return response(200, json!({}));
            }
            counter.set(counter.get() + 1);
            response(
                200,
                Value::Array(if counter.get() == 1 {
                    full.clone()
                } else {
                    tail.clone()
                }),
            )
        });
        let range = BarRange {
            from: Some(0),
            to: Some(2_000 * step),
            ..BarRange::default()
        };
        let bars = get_bars(&platform, &provider, "BTCUSDT", "1m", range);
        assert_eq!(bars.len(), 1010);
        let klines: Vec<String> = urls(&platform)
            .into_iter()
            .filter(|u| u.contains("/klines"))
            .collect();
        assert!(klines[0].contains("startTime=0&endTime=120000000"));
        assert!(klines[1].contains(&format!("startTime={}", 1000 * step)));
    }

    #[test]
    fn a_ranged_fetch_stops_at_the_limit() {
        let (platform, provider) = setup();
        let rows: Vec<Value> = (0..5).map(|i| kline(i * 1000, i * 1000 + 999)).collect();
        serve_klines(&platform, Value::Array(rows));
        let range = BarRange {
            from: Some(0),
            to: Some(10_000),
            limit: Some(3),
            ..BarRange::default()
        };
        assert_eq!(
            get_bars(&platform, &provider, "BTCUSDT", "1m", range).len(),
            3
        );
    }

    #[test]
    fn the_spot_endpoint_falls_back_from_com_to_us() {
        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("api.binance.com") && req.url.contains("/ping") {
                Some(Err(NetError::Failed("unreachable".to_string())))
            } else if req.url.contains("/ping") {
                response(200, json!({}))
            } else if req.url.contains("/klines") {
                response(200, json!([[0, "1", "2", "0", "1", "1", 1]]))
            } else {
                response(404, json!({}))
            }
        });
        get_bars(&platform, &provider, "BTCUSDT", "1h", limit(1));
        assert!(
            urls(&platform)
                .iter()
                .any(|u| u.contains("api.binance.us") && u.contains("/klines"))
        );
        let ping = platform
            .requests()
            .into_iter()
            .find(|r| r.url.contains("/ping"))
            .unwrap();
        assert_eq!(ping.timeout_ms, Some(5000.0));
    }

    #[test]
    fn concurrent_first_calls_share_one_probe_and_a_found_endpoint_is_cached() {
        let (platform, provider) = setup();
        serve_klines(&platform, json!([kline(0, 3_599_999)]));
        let a = provider.get_bars("BTCUSDT", "1h", &limit(1));
        let b = provider.get_bars("ETHUSDT", "1h", &limit(1));
        platform.block_on(async move {
            let _ = (a.await, b.await);
        });
        get_bars(&platform, &provider, "BTCUSDT", "1h", limit(1));
        let pings = urls(&platform)
            .into_iter()
            .filter(|u| u.contains("/ping"))
            .count();
        assert_eq!(pings, 1);
    }

    #[test]
    fn a_total_probe_failure_defaults_to_com_and_probes_again_next_time() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| Some(Err(NetError::Failed("down".to_string()))));
        get_bars(&platform, &provider, "BTCUSDT", "1h", limit(1));
        get_bars(&platform, &provider, "BTCUSDT", "1h", limit(1));
        let urls = urls(&platform);
        assert_eq!(urls.iter().filter(|u| u.contains("/ping")).count(), 4); // com + us, twice
        assert!(
            urls.iter()
                .any(|u| u.starts_with("https://api.binance.com/api/v3/klines"))
        );
    }

    // ── enumeration and symbol info ──

    fn exchange_info_routes(platform: &ManualPlatform) {
        let spot = json!({ "symbols": [
            { "symbol": "BTCUSDT", "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "USDT" },
            { "symbol": "OLDX", "status": "BREAK", "baseAsset": "OLD", "quoteAsset": "USDT" },
        ] });
        let futures = json!({ "symbols": [
            { "symbol": "BTCUSDT", "status": "TRADING", "contractType": "PERPETUAL", "baseAsset": "BTC", "quoteAsset": "USDT",
              "filters": [{ "filterType": "LOT_SIZE" }, { "filterType": "PRICE_FILTER", "tickSize": "0.10" }] },
            { "symbol": "ETHUSDT", "status": "TRADING", "contractType": "PERPETUAL", "baseAsset": "ETH", "quoteAsset": "USDT" },
            { "symbol": "BTCUSDT_240927", "status": "TRADING", "contractType": "CURRENT_QUARTER", "baseAsset": "BTC", "quoteAsset": "USDT" },
        ] });
        platform.on_fetch(move |req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else if req.url.contains("fapi") && req.url.contains("exchangeInfo") {
                response(200, futures.clone())
            } else if req.url.contains("exchangeInfo?symbol=BTCUSDT") {
                response(
                    200,
                    json!({ "symbols": [{ "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT",
                        "filters": [{ "filterType": "PRICE_FILTER", "tickSize": "0.01000000" }] }] }),
                )
            } else if req.url.contains("exchangeInfo") {
                response(200, spot.clone())
            } else {
                response(404, json!({}))
            }
        });
    }

    #[test]
    fn list_symbols_enumerates_trading_spot_and_perpetual_futures() {
        let (platform, provider) = setup();
        exchange_info_routes(&platform);
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let tickers: Vec<&str> = list.iter().map(|s| s.ticker.as_str()).collect();
        assert_eq!(tickers, vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT.P"]);
        assert_eq!(list[0].description.as_deref(), Some("BTC / USDT"));
        assert_eq!(list[0].kind.as_deref(), Some("crypto"));
        assert_eq!(list[1].description.as_deref(), Some("BTC / USDT Perpetual"));
        assert_eq!(list[1].kind.as_deref(), Some("futures"));
    }

    #[test]
    fn the_symbol_list_is_fetched_once_and_a_failed_half_is_left_out() {
        let (platform, provider) = setup();
        exchange_info_routes(&platform);
        let first = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        let before = platform.requests().len();
        let second = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        assert_eq!(first, second);
        assert_eq!(platform.requests().len(), before);

        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else if req.url.contains("fapi") {
                response(
                    200,
                    json!({ "symbols": [{ "symbol": "SOLUSDT", "status": "TRADING", "contractType": "PERPETUAL", "baseAsset": "SOL", "quoteAsset": "USDT" }] }),
                )
            } else {
                response(500, json!({}))
            }
        });
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        assert_eq!(
            list.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
            vec!["SOLUSDT.P"]
        );
    }

    #[test]
    fn symbol_info_reads_the_price_filter() {
        let (platform, provider) = setup();
        exchange_info_routes(&platform);
        let spot = platform
            .block_on(provider.get_symbol_info("BTCUSDT").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(spot.ticker, "BTCUSDT");
        assert_eq!(spot.timezone.as_deref(), Some("Etc/UTC"));
        assert_eq!(spot.session.as_deref(), Some("24x7"));
        assert_eq!(spot.extra["tickerid"], json!("BINANCE:BTCUSDT"));
        assert_eq!(spot.extra["prefix"], json!("BINANCE"));
        assert_eq!(spot.extra["description"], json!("BTC / USDT"));
        assert_eq!(spot.extra["type"], json!("crypto"));
        assert_eq!(spot.extra["basecurrency"], json!("BTC"));
        assert_eq!(spot.extra["currency"], json!("USDT"));
        assert_eq!(spot.extra["mintick"], json!(0.01));
        assert_eq!(spot.extra["pricescale"], json!(100));

        let perp = platform
            .block_on(provider.get_symbol_info("btcusdt.P").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(perp.ticker, "btcusdt.P"); // the original ticker, kept as typed
        assert_eq!(perp.extra["description"], json!("BTC / USDT Perpetual"));
        assert_eq!(perp.extra["type"], json!("futures"));
        assert_eq!(perp.extra["mintick"], json!(0.1));
        assert_eq!(perp.extra["pricescale"], json!(10));
    }

    #[test]
    fn symbol_info_is_absent_for_an_unknown_symbol_or_a_failed_request() {
        let (platform, provider) = setup();
        exchange_info_routes(&platform);
        let none = platform
            .block_on(provider.get_symbol_info("NOPEUSDT.P").unwrap())
            .unwrap();
        assert!(none.is_none());

        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                response(500, json!({}))
            }
        });
        let none = platform
            .block_on(provider.get_symbol_info("BTCUSDT").unwrap())
            .unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn the_provider_states_its_info_and_icon() {
        let (_, provider) = setup();
        let info = provider.info().unwrap();
        assert_eq!(info.name, "binance");
        assert_eq!(info.display_name.as_deref(), Some("Binance"));
        assert_eq!(info.requires_api_key, Some(false));
        assert!(
            info.capabilities.enumerate
                && info.capabilities.stream
                && info.capabilities.symbol_info
        );
        assert_eq!(info.supported_timeframes.unwrap().len(), 13);
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

    // ── subscribe ──

    type Bars = Rc<RefCell<Vec<Ohlcv>>>;

    fn collector() -> (Bars, BarSink) {
        let bars: Bars = Rc::new(RefCell::new(Vec::new()));
        let sink = bars.clone();
        (bars, Box::new(move |b| sink.borrow_mut().push(b)))
    }

    fn kline_frame(t: i64, close: &str) -> String {
        json!({ "e": "kline", "s": "BTCUSDT", "k": { "t": t, "o": "10", "h": "12", "l": "9", "c": close, "v": "3", "x": false } })
            .to_string()
    }

    #[test]
    fn a_perpetual_streams_klines_from_fstream_with_the_suffix_stripped() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        // the futures socket opens at once: no host probe
        let sockets = platform.sockets();
        assert_eq!(sockets.len(), 1);
        let ws = sockets[0].clone();
        assert!(ws.url.contains("fstream.binance.com"));
        assert!(ws.url.contains("btcusdt@kline_1h"));
        assert!(!ws.url.contains("btcusdt.p"));
        assert_eq!(ws.url, "wss://fstream.binance.com/ws/btcusdt@kline_1h");
        ws.emit_open();
        assert!(ws.sent().is_empty()); // the stream is in the URL: no subscribe frame
        ws.emit_message(&kline_frame(1000, "11"));
        assert_eq!(*bars.borrow(), vec![bar(1000, 10.0, 12.0, 9.0, 11.0, 3.0)]);
        unsub();
        assert!(ws.is_closed());
        // a frame after unsubscribe is dropped
        ws.emit_message(&kline_frame(2000, "11"));
        assert_eq!(bars.borrow().len(), 1);
    }

    #[test]
    fn frames_without_a_kline_are_ignored() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let _unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_message("not json");
        ws.emit_message("{\"result\":null,\"id\":1}");
        ws.emit_message("5");
        assert!(bars.borrow().is_empty());
    }

    #[test]
    fn the_spot_stream_picks_the_host_matching_the_resolved_rest_endpoint() {
        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                response(404, json!({}))
            }
        });
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT", "1h", sink, None).unwrap();
        assert!(platform.sockets().is_empty()); // waits for the host probe
        platform.run();
        let sockets = platform.sockets();
        assert_eq!(sockets.len(), 1);
        assert_eq!(
            sockets[0].url,
            "wss://stream.binance.com:9443/ws/btcusdt@kline_1h"
        );
        unsub();

        // api.binance.com unreachable: the .us stream host
        let (platform, provider) = setup();
        platform.on_fetch(|req| {
            if req.url.contains("api.binance.com") {
                Some(Err(NetError::Failed("blocked".to_string())))
            } else {
                response(200, json!({}))
            }
        });
        let (_bars, sink) = collector();
        let _unsub = provider.subscribe("BTCUSDT", "4h", sink, None).unwrap();
        platform.run();
        assert_eq!(
            platform.sockets()[0].url,
            "wss://stream.binance.us:9443/ws/btcusdt@kline_4h"
        );
    }

    #[test]
    fn unsubscribing_during_the_host_probe_opens_no_socket() {
        let (platform, provider) = setup();
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT", "1h", sink, None).unwrap();
        platform.run(); // the ping is pending
        assert_eq!(platform.pending_requests(), 1);
        unsub();
        platform.respond(
            "/ping",
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: "{}".into(),
            }),
        );
        platform.run();
        assert!(platform.sockets().is_empty());
    }

    #[test]
    fn an_aggregated_timeframe_polls_get_bars_every_three_seconds() {
        let (platform, provider) = setup();
        let rows: Vec<Value> = [0, 15, 30]
            .iter()
            .map(|m| json!([m * MIN, "1", "1", "1", "1", "1", m * MIN + 15 * MIN - 1]))
            .collect();
        serve_klines(&platform, Value::Array(rows));
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT", "45", sink, None).unwrap();
        assert!(platform.sockets().is_empty());
        platform.advance(2_999.0);
        assert!(bars.borrow().is_empty());
        platform.advance(2.0);
        let first = bars.borrow().len();
        assert!(first > 0);
        platform.advance(3_000.0);
        assert!(bars.borrow().len() > first); // the next poll cycle
        unsub();
        let after = bars.borrow().len();
        platform.advance(10_000.0);
        assert_eq!(bars.borrow().len(), after);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_silent_stream_falls_back_to_polling_after_the_stall_watchdog() {
        let (platform, provider) = setup();
        serve_klines(
            &platform,
            json!([
                [0, "1", "1", "1", "1", "1", 59_999],
                [60_000, "2", "2", "2", "2", "2", 119_999]
            ]),
        );
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        assert_eq!(platform.sockets().len(), 1);
        let ws = platform.sockets()[0].clone();
        ws.emit_open(); // opens but never delivers

        platform.advance(14_999.0);
        assert!(platform.warnings().is_empty());
        assert!(!ws.is_closed());
        platform.advance(1.0); // the 15 s watchdog fires
        assert!(ws.is_closed()); // the dead socket was closed
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Binance: BTCUSDT.P 1h stream delivered no data; falling back to polling."]
        );
        assert!(bars.borrow().is_empty());
        platform.advance(3_000.0); // a poll cycle
        assert!(!bars.borrow().is_empty());
        assert_eq!(platform.sockets().len(), 1); // no reconnect after the fallback
        platform.advance(10_000.0);
        assert_eq!(platform.sockets().len(), 1);
        unsub();
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn data_clears_the_watchdog() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        platform.advance(10_000.0);
        ws.emit_message(&kline_frame(1000, "11"));
        platform.advance(60_000.0);
        assert!(platform.warnings().is_empty());
        assert!(!ws.is_closed());
        assert_eq!(bars.borrow().len(), 1);
        unsub();
    }

    #[test]
    fn an_unexpected_close_reconnects_after_two_seconds_until_unsubscribed() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        let first = platform.sockets()[0].clone();
        first.emit_open();
        first.emit_message(&kline_frame(1000, "11"));
        first.emit_close();
        platform.advance(1_999.0);
        assert_eq!(platform.sockets().len(), 1);
        platform.advance(1.0);
        assert_eq!(platform.sockets().len(), 2);
        let second = platform.sockets()[1].clone();
        assert_eq!(second.url, first.url);
        second.emit_open();
        second.emit_message(&kline_frame(2000, "12"));
        assert_eq!(bars.borrow().len(), 2);

        // an error closes the socket; the close handler reconnects
        second.emit_error();
        assert!(second.is_closed());
        platform.advance(2_000.0);
        assert_eq!(platform.sockets().len(), 3);

        unsub();
        assert!(platform.sockets()[2].is_closed());
        platform.advance(60_000.0);
        assert_eq!(platform.sockets().len(), 3); // unsubscribed: no more reconnects
    }

    #[test]
    fn unsubscribing_with_a_reconnect_pending_cancels_it() {
        let (platform, provider) = setup();
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT.P", "1h", sink, None).unwrap();
        platform.sockets()[0].emit_close();
        unsub();
        platform.advance(60_000.0);
        assert_eq!(platform.sockets().len(), 1);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_poll_read_in_flight_at_unsubscribe_delivers_but_schedules_nothing_more() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTCUSDT", "45", sink, None).unwrap();
        // /ping is answered; the klines request stays pending
        platform.on_fetch(|req| {
            if req.url.contains("/ping") {
                response(200, json!({}))
            } else {
                None
            }
        });
        platform.advance(3_000.0);
        assert_eq!(platform.pending_requests(), 1);
        unsub();
        platform.respond(
            "/klines",
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: json!([[0, "1", "1", "1", "1", "1", 899_999]]).to_string(),
            }),
        );
        assert_eq!(bars.borrow().len(), 1);
        assert_eq!(platform.pending_timers(), 0);
    }

    // ── RateGate ──

    fn gate(platform: &ManualPlatform, max: usize) -> RateGate {
        RateGate::new(platform.handle(), max)
    }

    #[test]
    fn the_gate_never_runs_more_than_max_concurrent_tasks_at_once() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 2);
        let active = Rc::new(Cell::new(0));
        let peak = Rc::new(Cell::new(0));
        let finished = Rc::new(Cell::new(0));
        let mut releases = Vec::new();
        for _ in 0..4 {
            let (tx, rx) = oneshot::<()>();
            releases.push(tx);
            let (gate, active, peak, finished) =
                (gate.clone(), active.clone(), peak.clone(), finished.clone());
            platform.spawn(Box::pin(async move {
                let result = gate
                    .run(|| async {
                        active.set(active.get() + 1);
                        peak.set(peak.get().max(active.get()));
                        rx.await;
                        active.set(active.get() - 1);
                    })
                    .await;
                assert!(result.is_ok());
                finished.set(finished.get() + 1);
            }));
        }
        platform.run();
        assert_eq!(active.get(), 2); // only two acquired; the other two are queued
        releases[0].send(());
        releases[1].send(());
        platform.run();
        assert_eq!(active.get(), 2); // the two queued tasks took the freed slots
        releases[2].send(());
        releases[3].send(());
        platform.run();
        assert_eq!(finished.get(), 4);
        assert_eq!(peak.get(), 2);
    }

    fn run_at(platform: &ManualPlatform, gate: &RateGate) -> f64 {
        let (gate, p) = (gate.clone(), platform.handle());
        platform
            .block_on(async move { gate.run(|| async { p.now_ms() }).await })
            .unwrap()
    }

    #[test]
    fn the_gate_waits_out_a_pause_before_running() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 4);
        gate.pause_for(5000.0);
        assert_eq!(run_at(&platform, &gate), 5000.0);
    }

    #[test]
    fn a_response_near_the_weight_cap_throttles_to_the_next_minute() {
        let platform = ManualPlatform::new(10_000.0); // 10 s into a minute
        let gate = gate(&platform, 4);
        gate.note_weight(5000.0, 6000.0); // 5000 >= 0.8 * 6000 = 4800
        assert_eq!(run_at(&platform, &gate), 60_000.0); // next minute boundary
    }

    #[test]
    fn a_response_below_the_threshold_does_not_throttle() {
        let platform = ManualPlatform::new(10_000.0);
        let gate = gate(&platform, 4);
        gate.note_weight(1000.0, 6000.0); // well under 4800
        assert_eq!(run_at(&platform, &gate), 10_000.0);
        gate.note_weight(1.0, 0.0); // no known cap
        assert_eq!(run_at(&platform, &gate), 10_000.0);
    }

    #[test]
    fn a_custom_throttle_fraction_moves_the_threshold() {
        let platform = ManualPlatform::new(10_000.0);
        let gate = RateGate::with_throttle_fraction(platform.handle(), 4, 0.5);
        gate.note_weight(3000.0, 6000.0);
        assert_eq!(run_at(&platform, &gate), 60_000.0);
    }

    #[test]
    fn the_gate_fails_fast_while_banned_then_runs_again() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 4);
        gate.ban_for(30_000.0);
        let outcome = platform.block_on({
            let gate = gate.clone();
            async move { gate.run(|| async { 1 }).await }
        });
        let err = outcome.unwrap_err();
        assert!(err.banned);
        assert_eq!(err.message, "Binance IP banned for ~30s");
        assert_eq!(gate.ban_remaining_ms(), 30_000.0);
        platform.advance(30_000.0); // ban window elapsed
        let outcome = platform.block_on({
            let gate = gate.clone();
            async move { gate.run(|| async { 42 }).await }
        });
        assert_eq!(outcome, Ok(42));
        assert_eq!(gate.ban_remaining_ms(), 0.0);
    }

    #[test]
    fn a_ban_set_during_a_pause_fails_the_waiting_run() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 4);
        gate.pause_for(5000.0);
        let outcome = Rc::new(RefCell::new(None));
        let (g, slot) = (gate.clone(), outcome.clone());
        platform.spawn(Box::pin(async move {
            *slot.borrow_mut() = Some(g.run(|| async { 1 }).await);
        }));
        platform.run();
        gate.ban_for(10_000.0);
        platform.advance(5000.0);
        assert!(matches!(&*outcome.borrow(), Some(Err(e)) if e.banned));
    }

    #[test]
    fn a_dropped_task_frees_its_slot() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 1);
        let (_never, rx) = oneshot::<()>();
        let g = gate.clone();
        let mut stuck: LocalBoxFuture<'static, ()> = Box::pin(async move {
            let _ = g.run(|| rx).await;
        });
        // poll once so the slot is taken, then drop the future
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(stuck.as_mut().poll(&mut cx).is_pending());
        drop(stuck);
        // If the slot leaked, this would hang
        assert_eq!(
            platform.block_on({
                let gate = gate.clone();
                async move { gate.run(|| async { "ok" }).await }
            }),
            Ok("ok")
        );
    }

    #[test]
    fn the_gate_releases_its_slot_when_the_task_fails() {
        let platform = ManualPlatform::new(0.0);
        let gate = gate(&platform, 1);
        let failed: Result<Result<(), &str>, RateLimitError> = platform.block_on({
            let gate = gate.clone();
            async move { gate.run(|| async { Err("boom") }).await }
        });
        assert_eq!(failed, Ok(Err("boom")));
        // If the slot leaked, this second task would hang; it must resolve.
        let ok = platform.block_on({
            let gate = gate.clone();
            async move { gate.run(|| async { "ok" }).await }
        });
        assert_eq!(ok, Ok("ok"));
    }

    #[test]
    fn a_rate_limit_error_reads_as_a_data_error() {
        let e = RateLimitError {
            message: "banned".into(),
            banned: true,
        };
        assert_eq!(DataError::from(e), DataError::Other("banned".into()));
    }
}
