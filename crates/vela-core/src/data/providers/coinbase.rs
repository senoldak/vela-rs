//! Coinbase market-data provider, built on the public Exchange REST and WebSocket
//! APIs (no third-party SDK, no API key). Serves spot products (`BTC-USD`,
//! `ETH-EUR`), pages past the 300-candle cap, aggregates timeframes Coinbase does
//! not serve natively (`30`, `4h`) and folds `W`/`M` from daily candles. Live ticks
//! build a forming candle from the `ticker` stream (Coinbase has no native kline
//! stream), with a poll fallback.
//!
//! Also here: [`RequestGate`], the per-IP request gate of the REST path.
//!
//! The original's constructor takes no options. Its WebSocket-availability check
//! in `subscribe` is gone: a [`Platform`] always offers sockets. The 429 back-off
//! jitter reads `Platform::random`, the stand-in for `Math.random()`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;

use serde_json::Value;

use super::common::{
    FetchError, Interval, Memo, SharedSink, SymbolFields, aggregate_by, build_symbol_info,
    encode_query_value, is_present, is_truthy, js_num, js_parse_float, js_str_to_num, js_text,
    now_ms, parse_json, parse_minutes, poll_bars, select_sub_tf, tail,
};
use crate::data::{base_of, ledger_crypto_icon_url};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::{BarRange, BarSink, SymbolInfo};
use crate::runtime::{
    self, DataError, HttpRequest, HttpResponse, LocalBoxFuture, OneshotSender, Platform, Socket,
    SocketHandlers, TimerId, Unsubscribe, oneshot,
};

pub use super::common::{aggregate, dedupe_sorted, normalize_tf};

const REST_BASE: &str = "https://api.exchange.coinbase.com";
const WS_URL: &str = "wss://ws-feed.exchange.coinbase.com";
/// If a ticker socket opens but delivers no price within this window, treat it as
/// non-delivering and fall back to polling. Covers an illiquid product (no trades) or a
/// blocked endpoint that ACKs the subscription yet stays silent, so live updates never
/// go dead.
const STREAM_STALL_MS: f64 = 15_000.0;
/// Reconnect backoff after an unexpected socket close.
const STREAM_RECONNECT_MS: f64 = 2_000.0;
/// Coinbase has no native candle stream, so a forming bar is built from the `ticker`
/// channel (smooth price) re-seeded from REST candles on this cadence for an
/// authoritative open/volume.
const LIVE_RESEED_MS: f64 = 5_000.0;

/// Coinbase ranged-candle hard cap: at most 300 buckets per request (a wider range is an HTTP 400).
const MAX_CANDLES_PER_REQ: usize = 300;

/// Public REST shaping: a small concurrency cap plus min-spacing keeps the sustained rate under ~10/s.
const REST_CONCURRENCY: usize = 4;
const REST_MIN_INTERVAL_MS: f64 = 120.0;
/// How many times a single request retries after a 429 before giving up.
const REQUEST_MAX_RETRIES: u32 = 4;
/// Exponential-backoff base and jitter for a 429 retry when no Retry-After is given (ms).
const BACKOFF_BASE_MS: f64 = 600.0;
const BACKOFF_JITTER_MS: f64 = 400.0;

/// Native granularities in minutes (the aggregation sub-candidates Coinbase serves directly).
const NATIVE_MINUTES: [usize; 6] = [1, 5, 15, 60, 360, 1440];
const SUPPORTED_TIMEFRAMES: [&str; 16] = [
    "1", "3", "5", "15", "30", "45", "60", "120", "180", "240", "360", "480", "720", "D", "W", "M",
];

const MS_PER_DAY: i64 = 86_400_000;

/// One raw Coinbase candle row: `[time(sec), low, high, open, close, volume]` (low before high).
pub type RawCandle = Vec<Value>;

/// Canonical timeframe to Coinbase candle granularity (seconds). Native, no aggregation.
fn granularity_of(tf: &str) -> Option<i64> {
    Some(match tf {
        "1" => 60,
        "5" => 300,
        "15" => 900,
        "60" => 3600,
        "360" => 21600,
        "D" => 86400,
        _ => return None,
    })
}

/// Granularity (seconds) of a native sub-timeframe given in minutes.
fn minutes_to_granularity(minutes: usize) -> Option<i64> {
    granularity_of(&match minutes {
        1440 => "D".to_string(),
        m => m.to_string(),
    })
}

/// Map a Vela ticker to a Coinbase product id (`BTC-USD`). Upper-cased; whitespace trimmed.
pub fn parse_product_id(ticker: &str) -> String {
    ticker.trim().to_uppercase()
}

/// Map one raw Coinbase candle to a neutral bar. Row order is `[time, LOW, HIGH, open, close, volume]`.
pub fn candle_row_to_ohlcv(r: &[Value]) -> Ohlcv {
    Ohlcv::new(
        (js_num(r.first()) * 1000.0) as i64,
        js_num(r.get(3)),
        js_num(r.get(2)),
        js_num(r.get(1)),
        js_num(r.get(4)),
        Some(js_num(r.get(5))),
    )
}

/// The calendar unit `aggregate_calendar` folds into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalendarUnit {
    Week,
    Month,
}

/// Aggregate ascending daily candles into calendar weeks (Monday-aligned, UTC) or
/// calendar months (UTC). Coinbase has no native weekly/monthly granularity, so
/// `W`/`M` are folded from `1d`.
pub fn aggregate_calendar(sub: &[Ohlcv], unit: CalendarUnit) -> Vec<Ohlcv> {
    aggregate_by(
        sub,
        match unit {
            CalendarUnit::Week => week_start_utc,
            CalendarUnit::Month => month_start_utc,
        },
    )
}

/// Start of the Monday-aligned UTC week containing `ms`.
fn week_start_utc(ms: i64) -> i64 {
    let days = ms.div_euclid(MS_PER_DAY);
    let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday; 0 = Sunday
    let since_monday = (weekday + 6) % 7;
    (days - since_monday) * MS_PER_DAY
}

/// Start of the calendar UTC month containing `ms`.
fn month_start_utc(ms: i64) -> i64 {
    let (year, month, _) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    days_from_civil(year, month, 1) * MS_PER_DAY
}

/// Days since the epoch to a proleptic Gregorian `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// A proleptic Gregorian `(year, month, day)` to days since the epoch.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date.parse` for the ISO-8601 forms Coinbase sends (`2021-01-04T00:01:00.123456Z`),
/// plus a bare date and numeric offsets. A time without an offset is read as UTC.
fn parse_iso_ms(text: &str) -> Option<i64> {
    struct Cursor<'a> {
        bytes: &'a [u8],
        at: usize,
    }
    impl Cursor<'_> {
        fn eat(&mut self, byte: u8) -> bool {
            let hit = self.bytes.get(self.at) == Some(&byte);
            self.at += usize::from(hit);
            hit
        }
        fn digits(&mut self, count: usize) -> Option<i64> {
            let slice = self.bytes.get(self.at..self.at + count)?;
            if !slice.iter().all(u8::is_ascii_digit) {
                return None;
            }
            self.at += count;
            Some(slice.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
        }
    }

    let mut c = Cursor {
        bytes: text.trim().as_bytes(),
        at: 0,
    };
    let year = c.digits(4)?;
    if !c.eat(b'-') {
        return None;
    }
    let month = c.digits(2)?;
    if !c.eat(b'-') {
        return None;
    }
    let day = c.digits(2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (mut hour, mut minute, mut second, mut millis) = (0, 0, 0, 0);
    let mut offset_minutes = 0;
    if c.eat(b'T') || c.eat(b' ') {
        hour = c.digits(2)?;
        if !c.eat(b':') {
            return None;
        }
        minute = c.digits(2)?;
        if c.eat(b':') {
            second = c.digits(2)?;
            if c.eat(b'.') {
                let start = c.at;
                while c.bytes.get(c.at).is_some_and(u8::is_ascii_digit) {
                    c.at += 1;
                }
                if c.at == start {
                    return None;
                }
                let fraction: Vec<u8> = c.bytes[start..c.at].iter().copied().take(3).collect();
                millis = fraction
                    .iter()
                    .chain(std::iter::repeat(&b'0'))
                    .take(3)
                    .fold(0, |n, d| n * 10 + i64::from(d - b'0'));
            }
        }
        if hour > 24 || minute > 59 || second > 59 {
            return None;
        }
        if !c.eat(b'Z') {
            let sign = match c.bytes.get(c.at) {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => 0,
            };
            if sign != 0 {
                c.at += 1;
                let oh = c.digits(2)?;
                c.eat(b':');
                let om = c.digits(2)?;
                offset_minutes = sign * (oh * 60 + om);
            }
        }
    }
    if c.at != c.bytes.len() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * MS_PER_DAY + ((hour * 60 + minute - offset_minutes) * 60 + second) * 1000 + millis)
}

/// A `Retry-After` header (seconds) as ms, falling back to `fallback_ms` when absent/invalid.
fn retry_after_ms(res: &HttpResponse, fallback_ms: f64) -> f64 {
    let sec = res.header("retry-after").map_or(0.0, js_str_to_num);
    if sec.is_finite() && sec > 0.0 {
        sec * 1000.0
    } else {
        fallback_ms
    }
}

struct Inner {
    platform: Rc<dyn Platform>,
    /// Cached symbol enumeration (the products list is large; fetch once).
    symbols: Memo<Vec<SymbolDescriptor>>,
    /// Shared request gate: caps concurrency, spaces request starts, honors 429 backoff.
    gate: RequestGate,
}

/// The Coinbase provider: a cheap-to-clone handle.
#[derive(Clone)]
pub struct CoinbaseProvider {
    inner: Rc<Inner>,
}

impl CoinbaseProvider {
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self {
            inner: Rc::new(Inner {
                gate: RequestGate::new(platform.clone(), REST_CONCURRENCY, REST_MIN_INTERVAL_MS),
                platform,
                symbols: Memo::default(),
            }),
        }
    }
}

impl DataProvider for CoinbaseProvider {
    fn supports_stream(&self) -> bool {
        true
    }

    fn supports_symbol_info(&self) -> bool {
        true
    }

    fn info(&self) -> Option<ProviderInfo> {
        Some(ProviderInfo {
            name: "coinbase".to_string(),
            display_name: Some("Coinbase".to_string()),
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
                // A failed products request reads as no symbols.
                let products = inner.fetch_products().await.unwrap_or_default();
                products
                    .iter()
                    .filter(|p| {
                        p.get("status").and_then(Value::as_str) == Some("online")
                            && !is_truthy(p.get("trading_disabled"))
                    })
                    .map(|p| SymbolDescriptor {
                        ticker: js_text(p.get("id")),
                        description: Some(format!(
                            "{} / {}",
                            js_text(p.get("base_currency")),
                            js_text(p.get("quote_currency"))
                        )),
                        kind: Some("crypto".to_string()),
                        ..SymbolDescriptor::default()
                    })
                    .collect()
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
        let on_bar = SharedSink::new(on_bar);
        // A native granularity streams ticker-built candles; an aggregated timeframe or
        // W/M polls getBars.
        Some(match granularity_of(&normalize_tf(timeframe)) {
            Some(gran) => StreamCtx::start(inner, ticker, timeframe, gran, on_bar),
            None => inner.poll(ticker, timeframe, on_bar),
        })
    }
}

impl Inner {
    /// `getBars`, failing soft: empty plus a warning rather than a rejection.
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
                        "[vela] Coinbase: failed to fetch {ticker} {timeframe} — {e}"
                    ));
                    Vec::new()
                }
            }
        })
    }

    async fn try_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        let product = parse_product_id(ticker);
        let tf = normalize_tf(timeframe);

        if let Some(gran) = granularity_of(&tf) {
            return Ok(dedupe_sorted(
                &self.fetch_candles(&product, gran, range).await?,
            ));
        }

        // Calendar aggregation: W/M fold from daily candles (no weekly/monthly granularity).
        if tf == "W" || tf == "M" {
            let span_days = if tf == "W" { 7 } else { 31 };
            let sub_range = if range.from.is_some() {
                range.clone()
            } else {
                BarRange {
                    limit: range.limit.map(|l| l * span_days + 31),
                    ..range.clone()
                }
            };
            let sub = self.fetch_candles(&product, 86400, &sub_range).await?;
            let unit = if tf == "W" {
                CalendarUnit::Week
            } else {
                CalendarUnit::Month
            };
            return Ok(tail(aggregate_calendar(&sub, unit), range.limit));
        }

        // Numeric intraday aggregation from a native sub-timeframe.
        let target_min = parse_minutes(&tf);
        let sub_min = target_min.and_then(|t| select_sub_tf(&NATIVE_MINUTES, t));
        let (Some(target_min), Some(sub_min)) = (target_min, sub_min) else {
            self.platform.warn(&format!(
                "[vela] Coinbase: timeframe \"{timeframe}\" is not supported and cannot be aggregated."
            ));
            return Ok(Vec::new());
        };
        let ratio = target_min / sub_min;
        let sub_range = BarRange {
            limit: range.limit.map(|l| l * ratio + ratio),
            ..range.clone()
        };
        let gran = minutes_to_granularity(sub_min).unwrap_or(60);
        let sub = self.fetch_candles(&product, gran, &sub_range).await?;
        Ok(tail(
            aggregate(&sub, target_min as i64 * 60_000),
            range.limit,
        ))
    }

    async fn symbol_info(&self, ticker: &str) -> Option<SymbolInfo> {
        let product = parse_product_id(ticker);
        let p = self
            .json(&format!("{REST_BASE}/products/{product}"))
            .await
            .ok()?;
        if !is_truthy(p.get("id")) {
            return None;
        }
        let tick_size = match p.get("quote_increment") {
            Some(v) if is_present(Some(v)) => js_parse_float(&js_text(Some(v))),
            _ => 0.01,
        };
        let mintick = if tick_size > 0.0 { tick_size } else { 0.01 };
        Some(build_symbol_info(SymbolFields {
            venue: "COINBASE",
            ticker, // keep the original ticker (as Pine Script expects)
            description: format!(
                "{} / {}",
                js_text(p.get("base_currency")),
                js_text(p.get("quote_currency"))
            ),
            kind: "crypto",
            base: p.get("base_currency").cloned(),
            quote: p.get("quote_currency").cloned(),
            mintick,
        }))
    }

    async fn fetch_products(&self) -> Result<Vec<Value>, FetchError> {
        let data = self.json(&format!("{REST_BASE}/products")).await?;
        Ok(match data {
            Value::Array(products) => products,
            _ => Vec::new(),
        })
    }

    /// Poll the forming candle (aggregated/W/M timeframes).
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

    /// Fetch candles for a count (most-recent `limit`) or a `[from, to]` range, paging
    /// past the 300-bucket cap. Returns ascending bars (newest-first rows from the API
    /// are sorted on the way out).
    async fn fetch_candles(
        &self,
        product: &str,
        gran_sec: i64,
        range: &BarRange,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        if let Some(from) = range.from {
            let to = range.to.unwrap_or_else(|| now_ms(&self.platform));
            return self
                .paginate_forward(product, gran_sec, from, to, range.limit)
                .await;
        }
        let end = range.to.unwrap_or_else(|| now_ms(&self.platform));
        self.paginate_backward(product, gran_sec, range.limit.unwrap_or(500), end)
            .await
    }

    /// Assemble the most-recent `limit` bars, walking backward in windows of up to 300 buckets.
    async fn paginate_backward(
        &self,
        product: &str,
        gran_sec: i64,
        limit: usize,
        end_ms: i64,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        let gran_ms = gran_sec * 1000;
        let mut out: Vec<Ohlcv> = Vec::new();
        let mut remaining = limit as i64;
        let mut cursor_end = end_ms;
        let mut guard = limit.div_ceil(MAX_CANDLES_PER_REQ) + 4;
        while remaining > 0 && guard > 0 {
            guard -= 1;
            let size = remaining.min(MAX_CANDLES_PER_REQ as i64);
            let start_ms = cursor_end - size * gran_ms;
            let mut rows = self
                .candles_chunk(product, gran_sec, start_ms, cursor_end)
                .await?;
            let Some(first) = rows.first() else { break };
            cursor_end = first.time - 1; // earliest bar's open-time minus 1ms
            let count = rows.len() as i64;
            rows.append(&mut out);
            out = rows;
            remaining -= count;
            if count < size {
                break; // drained available history
            }
        }
        Ok(tail(dedupe_sorted(&out), Some(limit)))
    }

    /// Walk `[from, to]` forward in windows of up to 300 buckets (ranged/tail fetches).
    async fn paginate_forward(
        &self,
        product: &str,
        gran_sec: i64,
        from_ms: i64,
        to_ms: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        let gran_ms = gran_sec * 1000;
        let window = MAX_CANDLES_PER_REQ as i64 * gran_ms;
        let mut out: Vec<Ohlcv> = Vec::new();
        let mut cursor = from_ms;
        let mut guard = ((to_ms - from_ms).max(0) + window - 1) / window + 4;
        while cursor < to_ms && guard > 0 {
            guard -= 1;
            let window_end = (cursor + window).min(to_ms);
            out.extend(
                self.candles_chunk(product, gran_sec, cursor, window_end)
                    .await?,
            );
            cursor = window_end; // advance a full window; the one-bucket overlap is deduped out
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
        }
        let mut sorted = dedupe_sorted(&out);
        if let Some(l) = limit {
            sorted.truncate(l);
        }
        Ok(sorted)
    }

    /// One candles request over `[start_ms, end_ms]`: ascending bars (the API returns newest-first).
    async fn candles_chunk(
        &self,
        product: &str,
        gran_sec: i64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Ohlcv>, FetchError> {
        let url = format!(
            "{REST_BASE}/products/{product}/candles?granularity={}&start={}&end={}",
            encode_query_value(&gran_sec.to_string()),
            start_ms.div_euclid(1000),
            end_ms.div_euclid(1000)
        );
        let data = self.json(&url).await?;
        let Some(rows) = data.as_array() else {
            return Ok(Vec::new());
        };
        if rows.first().is_some_and(|row| !row.is_array()) {
            return Ok(Vec::new());
        }
        let mut bars: Vec<Ohlcv> = rows
            .iter()
            .map(|row| candle_row_to_ohlcv(row.as_array().map_or(&[], Vec::as_slice)))
            .collect();
        bars.sort_by_key(|b| b.time);
        Ok(bars)
    }

    /// GET to parsed JSON, through the rate gate with 429 backoff.
    async fn json(&self, url: &str) -> Result<Value, FetchError> {
        let res = self.request(url).await?;
        parse_json(&res)
    }

    /// Issue one GET through the shared gate (concurrency + spacing), retrying after a
    /// 429 (honoring `Retry-After`, else exponential backoff + jitter).
    async fn request(&self, url: &str) -> Result<HttpResponse, FetchError> {
        let mut attempt = 0;
        loop {
            let platform = self.platform.clone();
            let mut request = HttpRequest::get(url);
            request
                .headers
                .push(("Accept".to_string(), "application/json".to_string()));
            let res = self.gate.run(|| platform.fetch(request)).await?;
            if res.status == 429 && attempt < REQUEST_MAX_RETRIES {
                let backoff = BACKOFF_BASE_MS * f64::from(2u32.pow(attempt))
                    + self.platform.random() * BACKOFF_JITTER_MS;
                self.gate.pause_for(retry_after_ms(&res, 0.0).max(backoff));
                attempt += 1;
                continue;
            }
            if !res.ok() {
                return Err(FetchError::new(format!(
                    "Coinbase HTTP {} for {url}",
                    res.status
                )));
            }
            return Ok(res);
        }
    }
}

/// One live ticker subscription. Builds a forming candle from the Coinbase `ticker`
/// channel: the stream supplies a smooth live price/high/low, while a periodic REST
/// re-seed fixes the authoritative open and volume and corrects any drift. A stall
/// watchdog falls back to polling if the socket opens but never delivers a price;
/// it reconnects on an unexpected close until unsubscribed.
struct StreamCtx {
    provider: Rc<Inner>,
    ticker: String,
    timeframe: String,
    product: String,
    gran_ms: i64,
    on_bar: SharedSink,
    closed: Cell<bool>,
    ws: RefCell<Option<Rc<dyn Socket>>>,
    reconnect: Cell<Option<TimerId>>,
    stall: Cell<Option<TimerId>>,
    reseed_timer: RefCell<Option<Interval>>,
    /// Set once the stream is declared non-delivering and polling has taken over.
    polling: Cell<bool>,
    stop_polling: RefCell<Option<Unsubscribe>>,
    current: Cell<Option<Ohlcv>>,
}

impl StreamCtx {
    fn start(
        provider: Rc<Inner>,
        ticker: &str,
        timeframe: &str,
        gran_sec: i64,
        on_bar: SharedSink,
    ) -> Unsubscribe {
        let ctx = Rc::new(StreamCtx {
            provider,
            ticker: ticker.to_string(),
            timeframe: timeframe.to_string(),
            product: parse_product_id(ticker),
            gran_ms: gran_sec * 1000,
            on_bar,
            closed: Cell::new(false),
            ws: RefCell::new(None),
            reconnect: Cell::new(None),
            stall: Cell::new(None),
            reseed_timer: RefCell::new(None),
            polling: Cell::new(false),
            stop_polling: RefCell::new(None),
            current: Cell::new(None),
        });
        ctx.open();
        Box::new(move || {
            ctx.closed.set(true);
            ctx.clear_stall();
            ctx.clear_reseed();
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

    fn clear_reseed(&self) {
        let timer = self.reseed_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.clear();
        }
    }

    fn emit(&self) {
        if let Some(current) = self.current.get() {
            self.on_bar.emit(current);
        }
    }

    fn spawn_reseed(self: &Rc<Self>, emit_closed: bool) {
        let ctx = self.clone();
        self.provider
            .platform
            .spawn(Box::pin(async move { ctx.reseed(emit_closed).await }));
    }

    /// Pull the authoritative recent bars from REST; merge the forming one with the live extremes.
    async fn reseed(self: Rc<Self>, emit_closed: bool) {
        let bars = self
            .provider
            .clone()
            .bars(
                self.ticker.clone(),
                self.timeframe.clone(),
                BarRange {
                    limit: Some(2),
                    ..BarRange::default()
                },
            )
            .await;
        if self.closed.get() {
            return;
        }
        let [.., last] = bars.as_slice() else { return };
        let last = *last;
        if emit_closed && bars.len() >= 2 {
            self.on_bar.emit(bars[bars.len() - 2]); // the just-closed previous bar
        }
        let merged = match self.current.get() {
            Some(current) if current.time == last.time => Ohlcv {
                high: js_max(last.high, current.high),
                low: js_min(last.low, current.low),
                close: current.close,
                ..last
            },
            _ => last,
        };
        self.current.set(Some(merged));
        self.emit();
    }

    fn on_tick(self: &Rc<Self>, price: f64, t_ms: i64) {
        if !price.is_finite() {
            return;
        }
        let bar_start = t_ms.div_euclid(self.gran_ms) * self.gran_ms;
        match self.current.get() {
            Some(current) if bar_start == current.time => {
                self.current.set(Some(Ohlcv {
                    close: price,
                    high: js_max(current.high, price),
                    low: js_min(current.low, price),
                    ..current
                }));
                self.emit();
            }
            // A stale tick (bar_start < current.time) is ignored.
            Some(current) if bar_start < current.time => {}
            _ => {
                // Provisional until the reseed lands.
                self.current.set(Some(Ohlcv::new(
                    bar_start,
                    price,
                    price,
                    price,
                    price,
                    Some(0.0),
                )));
                self.emit();
                self.spawn_reseed(false); // fetch the authoritative open/volume for the new bar
            }
        }
    }

    fn fall_to_polling(self: &Rc<Self>) {
        if self.closed.get() || self.polling.get() {
            return;
        }
        self.clear_stall();
        self.clear_reseed();
        if let Some(id) = self.reconnect.take() {
            self.provider.platform.clear_timeout(id);
        }
        let ws = self.ws.borrow_mut().take();
        self.provider.platform.warn(&format!(
            "[vela] Coinbase: {} {} stream delivered no data; falling back to polling.",
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
                        "{{\"type\":\"subscribe\",\"product_ids\":[{}],\"channels\":[\"ticker\"]}}",
                        Value::from(ctx.product.as_str())
                    ));
                }
                ctx.spawn_reseed(false); // seed the forming bar now (don't wait for the first tick)
                ctx.clear_reseed();
                let tick = ctx.clone();
                *ctx.reseed_timer.borrow_mut() = Some(Interval::start(
                    &ctx.provider.platform,
                    LIVE_RESEED_MS,
                    move || tick.spawn_reseed(false),
                ));
            }
        };
        let on_message = {
            let ctx = self.clone();
            move |text: &str| {
                if ctx.closed.get() || ctx.polling.get() {
                    return;
                }
                // Non-JSON and unrelated frames are ignored.
                let Ok(msg) = serde_json::from_str::<Value>(text) else {
                    return;
                };
                let is_ours = msg.get("type").and_then(Value::as_str) == Some("ticker")
                    && msg.get("product_id").and_then(Value::as_str) == Some(ctx.product.as_str())
                    && is_present(msg.get("price"));
                if !is_ours {
                    return;
                }
                ctx.clear_stall();
                let t_ms = match msg.get("time") {
                    Some(time) if is_truthy(Some(time)) => parse_iso_ms(&js_text(Some(time))),
                    _ => Some(now_ms(&ctx.provider.platform)),
                };
                if let Some(t_ms) = t_ms {
                    ctx.on_tick(js_num(msg.get("price")), t_ms);
                }
            }
        };
        let on_close = {
            let ctx = self.clone();
            move || {
                ctx.clear_reseed();
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
            STREAM_STALL_MS, // watchdog: a price must arrive
            Box::new(move || ctx.fall_to_polling()),
        );
        self.stall.set(Some(id));
    }
}

struct GateState {
    platform: Rc<dyn Platform>,
    max_concurrent: usize,
    min_interval_ms: f64,
    active: Cell<usize>,
    waiters: RefCell<VecDeque<OneshotSender<()>>>,
    /// Earliest time the next request may START (min-spacing reservation).
    next_start_at: Cell<f64>,
    /// Backoff deadline set by a 429; new starts wait until then.
    paused_until: Cell<f64>,
}

/// A per-IP request gate for the Coinbase REST path. Coinbase Exchange throttles
/// public endpoints by request RATE (about 10 req/s sustained), not by a
/// per-response weight header like Binance, so this gate shapes throughput three ways:
///
///  - Concurrency cap: at most `max_concurrent` requests in flight, so overlapping
///    burst-prone backfills (initial + scroll + live poll) cannot fan out at once.
///  - Min spacing: request STARTS are spaced at least `min_interval_ms` apart,
///    holding the sustained rate under the public limit.
///  - Pause: a 429 sets a backoff ([`pause_for`](Self::pause_for)); new work waits it
///    out before starting.
///
/// One instance is shared across every REST call of a provider, so it is a
/// cheap-to-clone handle. It does no HTTP and no logging, and reads its clock and
/// timers from the platform. A slot is released when the task finishes or its
/// future is dropped.
#[derive(Clone)]
pub struct RequestGate {
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

impl RequestGate {
    pub fn new(platform: Rc<dyn Platform>, max_concurrent: usize, min_interval_ms: f64) -> Self {
        Self {
            state: Rc::new(GateState {
                platform,
                max_concurrent,
                min_interval_ms,
                active: Cell::new(0),
                waiters: RefCell::new(VecDeque::new()),
                next_start_at: Cell::new(0.0),
                paused_until: Cell::new(0.0),
            }),
        }
    }

    /// Acquire a slot, wait for the spacing/pause turn, run `f`, release.
    pub async fn run<T, F: Future<Output = T>>(&self, f: impl FnOnce() -> F) -> T {
        self.acquire().await;
        let _slot = Slot(self.state.clone());
        self.wait_for_turn().await;
        f().await
    }

    /// Pause new starts for `ms` (a 429 Retry-After / exponential backoff).
    pub fn pause_for(&self, ms: f64) {
        if ms > 0.0 {
            let until = self.state.platform.now_ms() + ms;
            self.state
                .paused_until
                .set(self.state.paused_until.get().max(until));
        }
    }

    /// Block until the spacing window opens and any pause has elapsed, then reserve the next slot.
    async fn wait_for_turn(&self) {
        let state = &self.state;
        loop {
            let now = state.platform.now_ms();
            let wait = (state.paused_until.get() - now).max(state.next_start_at.get() - now);
            if wait <= 0.0 {
                break;
            }
            // pausedUntil/nextStartAt may move while we sleep, so re-check
            runtime::sleep(&*state.platform, wait).await;
        }
        // Reserve this start; the next request cannot begin until a full interval later.
        state
            .next_start_at
            .set(state.platform.now_ms().max(state.next_start_at.get()) + state.min_interval_ms);
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
    use crate::runtime::NetError;
    use crate::runtime::testing::ManualPlatform;

    const MIN: i64 = 60_000;

    fn setup() -> (ManualPlatform, CoinbaseProvider) {
        let platform = ManualPlatform::new(10_000_000_000.0);
        let provider = CoinbaseProvider::new(platform.handle());
        (platform, provider)
    }

    fn response(status: u16, body: Value) -> Option<Result<HttpResponse, NetError>> {
        Some(Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }))
    }

    /// A raw Coinbase candle row: `[time(sec), low, high, open, close, volume]`.
    fn row(t_sec: i64) -> Value {
        json!([t_sec, 9, 12, 10, 11, 5])
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

    fn get_bars(
        platform: &ManualPlatform,
        provider: &CoinbaseProvider,
        ticker: &str,
        tf: &str,
        range: BarRange,
    ) -> Vec<Ohlcv> {
        platform
            .block_on(provider.get_bars(ticker, tf, &range))
            .unwrap()
    }

    fn urls(platform: &ManualPlatform) -> Vec<String> {
        platform.requests().into_iter().map(|r| r.url).collect()
    }

    /// `/candles` answers `rows`; anything else is a 404.
    fn serve_candles(platform: &ManualPlatform, rows: Vec<Value>) {
        platform.on_fetch(move |req| {
            if req.url.contains("/candles") {
                response(200, Value::Array(rows.clone()))
            } else {
                response(404, json!({}))
            }
        });
    }

    fn utc(y: i64, m: i64, d: i64) -> i64 {
        days_from_civil(y, m, d) * MS_PER_DAY
    }

    // ── pure helpers ──

    #[test]
    fn normalize_tf_maps_aliases_to_canonical_keys() {
        assert_eq!(normalize_tf("1h"), "60");
        assert_eq!(normalize_tf("4h"), "240");
        assert_eq!(normalize_tf("6h"), "360");
        assert_eq!(normalize_tf("1d"), "D");
        assert_eq!(normalize_tf("45m"), "45");
        assert_eq!(normalize_tf("60"), "60"); // already canonical
    }

    #[test]
    fn parse_product_id_upper_cases_and_trims() {
        assert_eq!(parse_product_id("btc-usd"), "BTC-USD");
        assert_eq!(parse_product_id("  eth-eur "), "ETH-EUR");
    }

    #[test]
    fn candle_rows_map_the_time_low_high_open_close_volume_tuple() {
        // note the API order: low precedes high; seconds become ms
        let r: Vec<Value> = [1000, 9, 12, 10, 11, 100]
            .iter()
            .map(|n| json!(n))
            .collect();
        assert_eq!(
            candle_row_to_ohlcv(&r),
            bar(1_000_000, 10.0, 12.0, 9.0, 11.0, 100.0)
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

    fn daily(times: &[i64]) -> Vec<Ohlcv> {
        times
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let i = i as f64;
                bar(*t, 10.0 + i, 20.0 + i, 5.0 + i, 15.0 + i, 1.0)
            })
            .collect()
    }

    #[test]
    fn aggregate_calendar_folds_daily_candles_into_monday_aligned_weeks() {
        let mon = utc(2021, 1, 4); // 2021-01-04 is a Monday
        let tue = utc(2021, 1, 5);
        let next_mon = utc(2021, 1, 11);
        let out = aggregate_calendar(&daily(&[mon, tue, next_mon]), CalendarUnit::Week);
        assert_eq!(
            out.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![mon, next_mon]
        );
        assert_eq!(out[0], bar(mon, 10.0, 21.0, 5.0, 16.0, 2.0)); // Mon+Tue folded
        // a Sunday belongs to the week that began the Monday before
        let sunday = utc(2021, 1, 10);
        let out = aggregate_calendar(&daily(&[sunday]), CalendarUnit::Week);
        assert_eq!(out[0].time, mon);
    }

    #[test]
    fn aggregate_calendar_folds_daily_candles_into_calendar_months() {
        let (jan1, jan15, feb1) = (utc(2021, 1, 1), utc(2021, 1, 15), utc(2021, 2, 1));
        let out = aggregate_calendar(&daily(&[jan1, jan15, feb1]), CalendarUnit::Month);
        assert_eq!(
            out.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![jan1, feb1]
        );
        assert_eq!(out[0].volume, Some(2.0)); // Jan 1 + Jan 15 folded
        // a year boundary and a leap day
        let out = aggregate_calendar(
            &daily(&[utc(2020, 12, 31), utc(2021, 1, 1)]),
            CalendarUnit::Month,
        );
        assert_eq!(out[0].time, utc(2020, 12, 1));
        let out = aggregate_calendar(&daily(&[utc(2020, 2, 29)]), CalendarUnit::Month);
        assert_eq!(out[0].time, utc(2020, 2, 1));
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(utc(1970, 1, 1), 0);
        assert_eq!(utc(2021, 1, 4), 1_609_718_400_000);
        for days in [-800_000, -1, 0, 59, 11_016, 19_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn iso_timestamps_parse_like_date_parse() {
        assert_eq!(
            parse_iso_ms("2021-01-04T00:00:00Z"),
            Some(1_609_718_400_000)
        );
        assert_eq!(
            parse_iso_ms("2021-01-04T00:01:00.123456Z"),
            Some(1_609_718_460_123)
        );
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.5Z"), Some(500));
        assert_eq!(
            parse_iso_ms("2021-01-04T01:00:00+01:00"),
            Some(1_609_718_400_000)
        );
        assert_eq!(parse_iso_ms("2021-01-04"), Some(1_609_718_400_000));
        assert_eq!(parse_iso_ms("2021-01-04T00:00Z"), Some(1_609_718_400_000));
        assert_eq!(parse_iso_ms("yesterday"), None);
        assert_eq!(parse_iso_ms("2021-13-04T00:00:00Z"), None);
        assert_eq!(parse_iso_ms("2021-01-04T00:00:00Zjunk"), None);
    }

    // ── getBars ──

    #[test]
    fn a_native_timeframe_is_fetched_directly_and_sorted_ascending() {
        let (platform, provider) = setup();
        // newest-first rows (as the API returns) at 0/60/120 min
        serve_candles(
            &platform,
            [120, 60, 0].iter().map(|m| row(m * 60)).collect(),
        );

        let bars = get_bars(&platform, &provider, "BTC-USD", "1h", limit(3));
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0], bar(0, 10.0, 12.0, 9.0, 11.0, 5.0));
        assert!(bars[0].time < bars[2].time);
        let calls = urls(&platform);
        assert!(
            calls
                .iter()
                .any(|u| u.contains("granularity=3600") && u.contains("/products/BTC-USD/candles"))
        );
        let req = &platform.requests()[0];
        assert_eq!(req.method, "GET");
        assert!(
            req.headers
                .contains(&("Accept".to_string(), "application/json".to_string()))
        );
    }

    #[test]
    fn an_unsupported_timeframe_aggregates_a_native_sub_timeframe() {
        // four 15m candles make two 30m bars ([0,30) and [30,60))
        let (platform, provider) = setup();
        serve_candles(
            &platform,
            [45, 30, 15, 0].iter().map(|m| row(m * 60)).collect(),
        );
        let bars = get_bars(&platform, &provider, "BTC-USD", "30", limit(2));
        assert!(
            urls(&platform)
                .iter()
                .any(|u| u.contains("granularity=900"))
        ); // requested 15m
        assert_eq!(
            bars.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![0, 30 * MIN]
        );
    }

    #[test]
    fn w_folds_from_daily_candles() {
        let (platform, provider) = setup();
        let sec = |y, m, d| utc(y, m, d) / 1000;
        // newest-first
        serve_candles(
            &platform,
            vec![
                row(sec(2021, 1, 11)),
                row(sec(2021, 1, 5)),
                row(sec(2021, 1, 4)),
            ],
        );
        let bars = get_bars(&platform, &provider, "BTC-USD", "1w", limit(2));
        assert!(
            urls(&platform)
                .iter()
                .any(|u| u.contains("granularity=86400"))
        );
        assert_eq!(
            bars.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![utc(2021, 1, 4), utc(2021, 1, 11)]
        );
    }

    #[test]
    fn m_asks_for_enough_daily_candles_and_folds_to_months() {
        let (platform, provider) = setup();
        let sec = |y, m, d| utc(y, m, d) / 1000;
        serve_candles(
            &platform,
            vec![
                row(sec(2021, 2, 1)),
                row(sec(2021, 1, 15)),
                row(sec(2021, 1, 1)),
            ],
        );
        let bars = get_bars(&platform, &provider, "BTC-USD", "1mo", limit(2));
        assert_eq!(
            bars.iter().map(|b| b.time).collect::<Vec<_>>(),
            vec![utc(2021, 1, 1), utc(2021, 2, 1)]
        );
        // 2 months at 31 days + 31 = 93 daily candles: one 300-bucket request window
        let first = &urls(&platform)[0];
        let start: i64 = first
            .split("start=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let end: i64 = first.split("end=").nth(1).unwrap().parse().unwrap();
        assert_eq!(end - start, 93 * 86_400);
    }

    #[test]
    fn range_from_is_the_forward_window_start() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![row(5000)]);
        let range = BarRange {
            from: Some(5_000_000),
            to: Some(5_100_000),
            ..BarRange::default()
        };
        get_bars(&platform, &provider, "BTC-USD", "1h", range);
        // start param is in seconds: range.from(ms) / 1000
        assert!(
            urls(&platform)
                .iter()
                .any(|u| u.contains("start=5000&end=5100"))
        );
    }

    #[test]
    fn a_forward_walk_advances_a_window_at_a_time_and_dedupes_the_overlap() {
        let (platform, provider) = setup();
        // 1m candles: a 300-bucket window is 18000 s; two windows over [0, 30000 s]
        platform.on_fetch(|req| {
            if req.url.contains("start=0&") {
                response(200, json!([row(18_000), row(17_940), row(0)]))
            } else {
                response(200, json!([row(30_000), row(18_000)]))
            }
        });
        let range = BarRange {
            from: Some(0),
            to: Some(30_000_000),
            ..BarRange::default()
        };
        let bars = get_bars(&platform, &provider, "BTC-USD", "1", range);
        assert_eq!(
            bars.iter().map(|b| b.time / 1000).collect::<Vec<_>>(),
            vec![0, 17_940, 18_000, 30_000]
        );
        assert_eq!(platform.requests().len(), 2);
    }

    #[test]
    fn a_failed_fetch_fails_soft_with_a_warning() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| response(503, json!({})));
        assert!(get_bars(&platform, &provider, "BTC-USD", "1h", limit(5)).is_empty());
        let warnings = platform.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("failed to fetch"));
        assert!(warnings[0].contains("Coinbase HTTP 503"));
    }

    #[test]
    fn malformed_json_and_a_network_error_fail_soft() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| {
            Some(Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: "oops".into(),
            }))
        });
        assert!(get_bars(&platform, &provider, "BTC-USD", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].contains("failed to fetch BTC-USD 1h"));

        let (platform, provider) = setup();
        platform.on_fetch(|_| Some(Err(NetError::Failed("offline".into()))));
        assert!(get_bars(&platform, &provider, "BTC-USD", "1h", limit(5)).is_empty());
        assert!(platform.warnings()[0].ends_with("— offline"));
    }

    #[test]
    fn a_timeframe_that_cannot_be_aggregated_warns() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![]);
        assert!(get_bars(&platform, &provider, "BTC-USD", "x", limit(3)).is_empty());
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Coinbase: timeframe \"x\" is not supported and cannot be aggregated."]
        );
    }

    #[test]
    fn pagination_walks_backward_past_the_300_candle_cap() {
        let (platform, provider) = setup();
        let step = 3600;
        // page 1 = most-recent 300, page 2 = older 200 (newest-first within each page)
        let page1: Vec<Value> = (0..300).map(|i| row((1000 + 299 - i) * step)).collect();
        let page2: Vec<Value> = (0..200).map(|i| row((800 + 199 - i) * step)).collect();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |_| {
            counter.set(counter.get() + 1);
            match counter.get() {
                1 => response(200, Value::Array(page1.clone())),
                2 => response(200, Value::Array(page2.clone())),
                _ => response(200, json!([])),
            }
        });
        let bars = get_bars(&platform, &provider, "BTC-USD", "1h", limit(500));
        assert_eq!(calls.get(), 2); // 300 + 200
        assert_eq!(bars.len(), 500);
        assert!(bars[0].time < bars[bars.len() - 1].time);
        // the second window ends one ms (floored to the second) before the first page's oldest bar
        let second = &urls(&platform)[1];
        assert!(second.ends_with(&format!("end={}", 1000 * step - 1)));
    }

    // ── gated requests ──

    #[test]
    fn requests_are_spaced_by_the_gate() {
        let (platform, provider) = setup();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |_| {
            counter.set(counter.get() + 1);
            let rows: Vec<Value> = if counter.get() == 1 {
                (0..300).map(|i| row((1000 + i) * 3600)).collect()
            } else {
                vec![row(0)]
            };
            response(200, Value::Array(rows))
        });
        let start = platform.now_ms();
        get_bars(&platform, &provider, "BTC-USD", "1h", limit(600)); // two windows
        assert_eq!(platform.requests().len(), 2);
        assert_eq!(platform.now_ms() - start, 120.0); // one min-interval between starts
    }

    fn rate_limited(retry_after: Option<&str>) -> Option<Result<HttpResponse, NetError>> {
        Some(Ok(HttpResponse {
            status: 429,
            headers: retry_after
                .map(|v| vec![("retry-after".to_string(), v.to_string())])
                .unwrap_or_default(),
            body: "{}".into(),
        }))
    }

    #[test]
    fn a_429_is_retried_after_the_retry_after_delay() {
        let (platform, provider) = setup();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |_| {
            counter.set(counter.get() + 1);
            if counter.get() == 1 {
                rate_limited(Some("2"))
            } else {
                response(200, json!([row(0)]))
            }
        });
        let start = platform.now_ms();
        let bars = get_bars(&platform, &provider, "BTC-USD", "1h", limit(1));
        assert_eq!(bars.len(), 1);
        assert_eq!(calls.get(), 2);
        assert!(platform.now_ms() - start >= 2000.0);
        assert!(platform.warnings().is_empty());
    }

    #[test]
    fn a_429_without_retry_after_backs_off_exponentially_with_jitter() {
        let (platform, provider) = setup();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        platform.on_fetch(move |_| {
            counter.set(counter.get() + 1);
            if counter.get() <= 2 {
                rate_limited(None)
            } else {
                response(200, json!([row(0)]))
            }
        });
        let start = platform.now_ms();
        get_bars(&platform, &provider, "BTC-USD", "1h", limit(1));
        assert_eq!(calls.get(), 3);
        // 600 * 2^0 + jitter, then 600 * 2^1 + jitter, each jitter below 400
        let waited = platform.now_ms() - start;
        assert!((1800.0..2600.0).contains(&waited), "waited {waited}");
    }

    #[test]
    fn a_429_gives_up_after_four_retries() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| rate_limited(Some("1")));
        assert!(get_bars(&platform, &provider, "BTC-USD", "1h", limit(1)).is_empty());
        assert_eq!(platform.requests().len(), 5); // the first try and four retries
        assert!(platform.warnings()[0].contains("Coinbase HTTP 429"));
    }

    // ── enumeration and symbol info ──

    #[test]
    fn list_symbols_keeps_online_trading_enabled_products() {
        let (platform, provider) = setup();
        let products = json!([
            { "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD", "status": "online", "trading_disabled": false },
            { "id": "ETH-EUR", "base_currency": "ETH", "quote_currency": "EUR", "status": "online", "trading_disabled": false },
            { "id": "OLD-USD", "base_currency": "OLD", "quote_currency": "USD", "status": "delisted", "trading_disabled": false },
            { "id": "HALT-USD", "base_currency": "HALT", "quote_currency": "USD", "status": "online", "trading_disabled": true },
        ]);
        platform.on_fetch(move |req| {
            if req.url.ends_with("/products") {
                response(200, products.clone())
            } else {
                response(404, json!({}))
            }
        });
        let list = platform.block_on(provider.list_symbols().unwrap()).unwrap();
        assert_eq!(
            list.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
            vec!["BTC-USD", "ETH-EUR"]
        );
        assert_eq!(list[0].description.as_deref(), Some("BTC / USD"));
        assert_eq!(list[0].kind.as_deref(), Some("crypto"));
        // fetched once
        let before = platform.requests().len();
        platform.block_on(provider.list_symbols().unwrap()).unwrap();
        assert_eq!(platform.requests().len(), before);
    }

    #[test]
    fn a_failed_or_malformed_products_list_reads_as_empty() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| response(500, json!({})));
        assert!(
            platform
                .block_on(provider.list_symbols().unwrap())
                .unwrap()
                .is_empty()
        );
        let (platform, provider) = setup();
        platform.on_fetch(|_| response(200, json!({ "message": "nope" })));
        assert!(
            platform
                .block_on(provider.list_symbols().unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn symbol_info_derives_mintick_from_quote_increment() {
        let (platform, provider) = setup();
        let product = json!({ "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD",
            "quote_increment": "0.01", "base_increment": "0.00000001", "status": "online" });
        platform.on_fetch(move |req| {
            if req.url.ends_with("/products/BTC-USD") {
                response(200, product.clone())
            } else {
                response(404, json!({}))
            }
        });
        let info = platform
            .block_on(provider.get_symbol_info("BTC-USD").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.ticker, "BTC-USD");
        assert_eq!(info.extra["tickerid"], json!("COINBASE:BTC-USD"));
        assert_eq!(info.extra["prefix"], json!("COINBASE"));
        assert_eq!(info.extra["type"], json!("crypto"));
        assert_eq!(info.extra["description"], json!("BTC / USD"));
        assert_eq!(info.extra["basecurrency"], json!("BTC"));
        assert_eq!(info.extra["currency"], json!("USD"));
        assert_eq!(info.extra["mintick"], json!(0.01));
        assert_eq!(info.extra["pricescale"], json!(100));
        assert_eq!(info.timezone.as_deref(), Some("Etc/UTC"));
        assert_eq!(info.session.as_deref(), Some("24x7"));
    }

    #[test]
    fn symbol_info_is_absent_for_an_unknown_product_and_defaults_a_bad_increment() {
        let (platform, provider) = setup();
        platform.on_fetch(|_| response(404, json!({ "message": "NotFound" })));
        assert!(
            platform
                .block_on(provider.get_symbol_info("NOPE-USD").unwrap())
                .unwrap()
                .is_none()
        );

        let (platform, provider) = setup();
        platform.on_fetch(|_| {
            response(200, json!({ "id": "X-USD", "base_currency": "X", "quote_currency": "USD", "quote_increment": "0" }))
        });
        let info = platform
            .block_on(provider.get_symbol_info("x-usd").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(info.extra["mintick"], json!(0.01));
        assert_eq!(info.extra["pricescale"], json!(100));
        assert_eq!(info.ticker, "x-usd"); // the original, as typed
    }

    #[test]
    fn the_provider_states_its_info_and_icon() {
        let (_, provider) = setup();
        let info = provider.info().unwrap();
        assert_eq!(info.name, "coinbase");
        assert_eq!(info.display_name.as_deref(), Some("Coinbase"));
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

    // ── subscribe ──

    type Bars = Rc<RefCell<Vec<Ohlcv>>>;

    fn collector() -> (Bars, BarSink) {
        let bars: Bars = Rc::new(RefCell::new(Vec::new()));
        let sink = bars.clone();
        (bars, Box::new(move |b| sink.borrow_mut().push(b)))
    }

    fn iso(ms: i64) -> String {
        let days = ms.div_euclid(MS_PER_DAY);
        let (y, m, d) = civil_from_days(days);
        let rem = ms.rem_euclid(MS_PER_DAY);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
            rem / 3_600_000,
            rem / 60_000 % 60,
            rem / 1000 % 60,
            rem % 1000
        )
    }

    fn ticker_frame(price: &str, ms: i64) -> String {
        json!({ "type": "ticker", "product_id": "BTC-USD", "price": price, "time": iso(ms) })
            .to_string()
    }

    #[test]
    fn a_forming_bar_is_built_from_the_ticker_channel() {
        let (platform, provider) = setup();
        // reseed asks getBars for /candles: one 1h bar at 3,600,000 ms is the base
        serve_candles(&platform, vec![row(3600)]);
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        let sockets = platform.sockets();
        assert_eq!(sockets.len(), 1);
        let ws = sockets[0].clone();
        assert_eq!(ws.url, "wss://ws-feed.exchange.coinbase.com");
        ws.emit_open();
        assert_eq!(
            ws.sent(),
            vec![
                "{\"type\":\"subscribe\",\"product_ids\":[\"BTC-USD\"],\"channels\":[\"ticker\"]}"
            ]
        );

        // a ticker tick within the 1h bar [3,600,000, 7,200,000) updates close/high/low
        ws.emit_message(&ticker_frame("105", 3_600_000 + MIN));
        let formed = *bars.borrow().last().unwrap();
        assert_eq!(formed, bar(3_600_000, 105.0, 105.0, 105.0, 105.0, 0.0)); // provisional
        platform.advance(200.0); // the reseed lands and merges the live extremes
        let merged = *bars.borrow().last().unwrap();
        assert_eq!(merged, bar(3_600_000, 10.0, 105.0, 9.0, 105.0, 5.0));
        // a lower tick extends the low, a stale tick is ignored
        ws.emit_message(&ticker_frame("8", 3_600_000 + 2 * MIN));
        let low = *bars.borrow().last().unwrap();
        assert_eq!((low.low, low.close, low.high), (8.0, 8.0, 105.0));
        let count = bars.borrow().len();
        ws.emit_message(&ticker_frame("50", 3_599_000));
        assert_eq!(bars.borrow().len(), count);
        // other products and non-ticker frames are ignored
        ws.emit_message(
            &json!({ "type": "ticker", "product_id": "ETH-USD", "price": "5" }).to_string(),
        );
        ws.emit_message(&json!({ "type": "subscriptions", "channels": [] }).to_string());
        ws.emit_message("not json");
        assert_eq!(bars.borrow().len(), count);
        unsub();
        assert!(ws.is_closed());
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_new_bar_period_starts_a_fresh_forming_bar() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![]); // reseeds find nothing
        let (bars, sink) = collector();
        let _unsub = provider.subscribe("BTC-USD", "1", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        ws.emit_message(&ticker_frame("10", MIN + 5));
        ws.emit_message(&ticker_frame("12", MIN + 6));
        ws.emit_message(&ticker_frame("11", 2 * MIN + 1));
        let all = bars.borrow().clone();
        assert_eq!(
            all,
            vec![
                bar(MIN, 10.0, 10.0, 10.0, 10.0, 0.0),
                bar(MIN, 10.0, 12.0, 10.0, 12.0, 0.0),
                bar(2 * MIN, 11.0, 11.0, 11.0, 11.0, 0.0),
            ]
        );
    }

    #[test]
    fn the_stream_reseeds_from_rest_every_five_seconds_while_open() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![row(3600)]);
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        platform.advance(1.0);
        assert_eq!(platform.requests().len(), 1); // the immediate seed
        assert_eq!(bars.borrow().len(), 1);
        platform.advance(5_000.0);
        assert_eq!(platform.requests().len(), 2);
        platform.advance(5_000.0);
        assert_eq!(platform.requests().len(), 3);
        ws.emit_close(); // the reseed timer stops with the socket
        let requests = platform.requests().len();
        platform.advance(1_000.0);
        unsub();
        platform.advance(20_000.0);
        assert_eq!(platform.requests().len(), requests);
    }

    #[test]
    fn a_silent_stream_falls_back_to_polling_after_the_stall_watchdog() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![row(0), row(3600)]);
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        platform.advance(14_999.0);
        assert!(platform.warnings().is_empty());
        platform.advance(1.0);
        assert!(ws.is_closed());
        assert_eq!(
            platform.warnings(),
            vec!["[vela] Coinbase: BTC-USD 1h stream delivered no data; falling back to polling."]
        );
        let before = bars.borrow().len();
        platform.advance(3_000.0);
        assert!(bars.borrow().len() > before);
        assert_eq!(platform.sockets().len(), 1); // no reconnect after the fallback
        unsub();
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn a_price_clears_the_watchdog() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![]);
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        platform.advance(10_000.0);
        ws.emit_message(&ticker_frame("105", 3_600_000));
        platform.advance(60_000.0);
        assert!(platform.warnings().is_empty());
        assert!(!ws.is_closed());
        unsub();
    }

    #[test]
    fn an_unexpected_close_reconnects_after_two_seconds_until_unsubscribed() {
        let (platform, provider) = setup();
        serve_candles(&platform, vec![]);
        let (_bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        platform.sockets()[0].emit_open();
        platform.sockets()[0].emit_close();
        platform.advance(1_999.0);
        assert_eq!(platform.sockets().len(), 1);
        platform.advance(1.0);
        assert_eq!(platform.sockets().len(), 2);
        platform.sockets()[1].emit_open();
        assert_eq!(platform.sockets()[1].sent().len(), 1); // it subscribes again
        unsub();
        assert!(platform.sockets()[1].is_closed());
        platform.advance(60_000.0);
        assert_eq!(platform.sockets().len(), 2);
    }

    #[test]
    fn a_reseed_that_lands_after_unsubscribe_is_dropped() {
        let (platform, provider) = setup();
        let (bars, sink) = collector();
        let unsub = provider.subscribe("BTC-USD", "1h", sink, None).unwrap();
        platform.on_fetch(|_| None); // requests stay pending
        let ws = platform.sockets()[0].clone();
        ws.emit_open();
        platform.run();
        assert_eq!(platform.pending_requests(), 1);
        unsub();
        platform.respond(
            "/candles",
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: json!([row(3600)]).to_string(),
            }),
        );
        assert!(bars.borrow().is_empty());
    }

    #[test]
    fn w_m_and_aggregated_timeframes_poll() {
        for tf in ["W", "30", "1mo"] {
            let (platform, provider) = setup();
            serve_candles(&platform, vec![row(0), row(86_400)]);
            let (bars, sink) = collector();
            let unsub = provider.subscribe("BTC-USD", tf, sink, None).unwrap();
            assert!(platform.sockets().is_empty());
            platform.advance(3_100.0);
            assert!(!bars.borrow().is_empty(), "{tf}");
            unsub();
        }
    }

    // ── RequestGate ──

    #[test]
    fn the_gate_spaces_consecutive_request_starts() {
        let platform = ManualPlatform::new(1000.0);
        let gate = RequestGate::new(platform.handle(), 10, 100.0);
        let mut starts = Vec::new();
        for _ in 0..3 {
            let (g, p) = (gate.clone(), platform.handle());
            starts.push(platform.block_on(async move { g.run(|| async { p.now_ms() }).await }));
        }
        assert_eq!(starts[1] - starts[0], 100.0);
        assert_eq!(starts[2] - starts[1], 100.0); // each consecutive start spaced one interval
    }

    #[test]
    fn the_gate_caps_concurrency_and_queues_the_excess() {
        let platform = ManualPlatform::new(1000.0);
        let gate = RequestGate::new(platform.handle(), 2, 0.0); // no spacing: isolate the cap
        let active = Rc::new(Cell::new(0));
        let peak = Rc::new(Cell::new(0));
        let done = Rc::new(Cell::new(0));
        let mut releases = Vec::new();
        for _ in 0..4 {
            let (tx, rx) = oneshot::<()>();
            releases.push(tx);
            let (gate, active, peak, done) =
                (gate.clone(), active.clone(), peak.clone(), done.clone());
            platform.spawn(Box::pin(async move {
                gate.run(|| async {
                    active.set(active.get() + 1);
                    peak.set(peak.get().max(active.get()));
                    rx.await;
                    active.set(active.get() - 1);
                })
                .await;
                done.set(done.get() + 1);
            }));
        }
        platform.run();
        assert_eq!(peak.get(), 2); // only two ran; the other two are queued at the gate
        for release in &releases {
            release.send(());
            platform.run();
        }
        assert_eq!(done.get(), 4);
        assert_eq!(peak.get(), 2);
    }

    #[test]
    fn pause_for_holds_the_next_start_until_the_backoff_elapses() {
        let platform = ManualPlatform::new(1000.0);
        let gate = RequestGate::new(platform.handle(), 10, 0.0);
        let at = |platform: &ManualPlatform, gate: &RequestGate| {
            let (g, p) = (gate.clone(), platform.handle());
            platform.block_on(async move { g.run(|| async { p.now_ms() }).await })
        };
        let first = at(&platform, &gate);
        gate.pause_for(500.0);
        let second = at(&platform, &gate);
        assert_eq!(second - first, 500.0);
        gate.pause_for(0.0); // a non-positive pause does nothing
        assert_eq!(at(&platform, &gate), second);
    }

    #[test]
    fn a_dropped_task_frees_its_slot() {
        let platform = ManualPlatform::new(0.0);
        let gate = RequestGate::new(platform.handle(), 1, 0.0);
        let (_keep, rx) = oneshot::<()>();
        let g = gate.clone();
        let mut stuck: LocalBoxFuture<'static, ()> = Box::pin(async move {
            g.run(|| rx).await;
        });
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(stuck.as_mut().poll(&mut cx).is_pending());
        drop(stuck);
        let g = gate.clone();
        assert_eq!(
            platform.block_on(async move { g.run(|| async { "ok" }).await }),
            "ok"
        );
    }

    #[test]
    fn retry_after_reads_seconds_and_falls_back() {
        let with = |value: Option<&str>| HttpResponse {
            status: 429,
            headers: value
                .map(|v| vec![("retry-after".to_string(), v.to_string())])
                .unwrap_or_default(),
            body: String::new(),
        };
        assert_eq!(retry_after_ms(&with(Some("3")), 7.0), 3000.0);
        assert_eq!(retry_after_ms(&with(Some("0")), 7.0), 7.0);
        assert_eq!(retry_after_ms(&with(Some("soon")), 7.0), 7.0);
        assert_eq!(retry_after_ms(&with(None), 7.0), 7.0);
    }
}
