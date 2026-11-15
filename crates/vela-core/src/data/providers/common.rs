//! Helpers the three bundled providers share. The TypeScript original copies
//! them into each provider; here they live once.
//!
//! Besides the pure bar helpers (`dedupe_sorted`, `aggregate`) and the JavaScript
//! number coercions the exchange payloads rely on, this holds the small async
//! building blocks the providers need: a cached shared result ([`Memo`], the
//! original's cached `Promise` field), joining futures, the poll fallback of a
//! live subscription, and a repeating timer.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::future::{Future, poll_fn};
use std::rc::Rc;
use std::task::Poll;

use serde_json::{Map, Number, Value};

use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::ports::market_data::{BarSink, SymbolInfo};
use crate::runtime::{
    HttpResponse, LocalBoxFuture, NetError, OneshotSender, Platform, TimerId, Unsubscribe, oneshot,
};

/// Why a provider request failed: a network error, an HTTP error status or a
/// malformed body. Providers log it and read it as an empty answer. (A small
/// stand-in for the port's `DataError`, which is large to pass around.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError(String);

impl FetchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<NetError> for FetchError {
    fn from(e: NetError) -> Self {
        Self(e.to_string())
    }
}

/// How often a poll fallback re-reads the forming candle.
const POLL_MS: f64 = 3000.0;

// ── JavaScript coercions ──

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `Number(text)`: empty is 0, anything that is not a whole numeric literal is NaN.
pub fn js_str_to_num(text: &str) -> f64 {
    let t = text.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if t.bytes()
        .all(|b| b.is_ascii_digit() || b".eE+-".contains(&b))
    {
        return t.parse().unwrap_or(f64::NAN);
    }
    let radix = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0b" | "0B") => 2,
        Some("0o" | "0O") => 8,
        _ => return f64::NAN,
    };
    u64::from_str_radix(&t[2..], radix).map_or(f64::NAN, |n| n as f64)
}

/// `Number(value)` for a JSON value; a missing one is `undefined`, so NaN.
pub fn js_num(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => js_str_to_num(s),
        Some(Value::Null) | Some(Value::Bool(false)) => 0.0,
        Some(Value::Bool(true)) => 1.0,
        Some(_) => f64::NAN,
    }
}

/// `parseFloat(text)`: the longest numeric prefix, NaN when there is none.
pub fn js_parse_float(text: &str) -> f64 {
    let t = text.trim_start_matches(is_js_space);
    let (sign, rest) = match t.as_bytes().first() {
        Some(b'-') => (-1.0, &t[1..]),
        Some(b'+') => (1.0, &t[1..]),
        _ => (1.0, t),
    };
    if rest.starts_with("Infinity") {
        return sign * f64::INFINITY;
    }
    let bytes = rest.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let int_len = digits(0);
    let mut end = int_len;
    let mut mantissa_digits = int_len;
    if bytes.get(end) == Some(&b'.') {
        let frac_len = digits(end + 1);
        mantissa_digits += frac_len;
        if frac_len > 0 || int_len > 0 {
            end += 1 + frac_len;
        }
    }
    if mantissa_digits == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut at = end + 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exp_len = digits(at);
        if exp_len > 0 {
            end = at + exp_len;
        }
    }
    sign * rest[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// What a JavaScript template literal prints for a JSON value (`${value}`).
pub fn js_text(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n
            .as_f64()
            .map_or_else(|| n.to_string(), crate::util::js_number),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A number as a JSON value: whole numbers as integers (so `100` equals `json!(100)`),
/// anything else as a float, and a non-finite one as `null` (JSON has no NaN).
pub fn num_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}

/// `Boolean(value)` of an optional JSON field.
pub fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `value != null` on an optional JSON field.
pub fn is_present(value: Option<&Value>) -> bool {
    !matches!(value, None | Some(Value::Null))
}

/// `encodeURIComponent`-like escaping of a query value, the way `URLSearchParams`
/// writes it (`application/x-www-form-urlencoded`: a space is `+`).
pub fn encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'*' | b'-' | b'.' | b'_' => out.push(char::from(b)),
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() => out.push(char::from(b)),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The response body as JSON; a malformed body fails the request.
pub fn parse_json(res: &HttpResponse) -> Result<Value, FetchError> {
    res.json().map_err(|e| FetchError::new(e.to_string()))
}

/// `Date.now()` as whole epoch ms.
pub fn now_ms(platform: &Rc<dyn Platform>) -> i64 {
    platform.now_ms() as i64
}

// ── timeframes and bars ──

/// User-facing timeframe alias to its canonical key (shared by every Vela layer).
fn canonical_tf(key: &str) -> Option<&'static str> {
    Some(match key {
        "1m" => "1",
        "3m" => "3",
        "5m" => "5",
        "15m" => "15",
        "30m" => "30",
        "45m" => "45",
        "1h" => "60",
        "2h" => "120",
        "3h" => "180",
        "4h" => "240",
        "6h" => "360",
        "8h" => "480",
        "12h" => "720",
        "1d" => "D",
        "1w" => "W",
        "1mo" => "M",
        "1D" => "D",
        "1W" => "W",
        "4H" => "240",
        "D" => "D",
        "W" => "W",
        "M" => "M",
        _ => return None,
    })
}

/// Normalize a user timeframe to a canonical key.
pub fn normalize_tf(tf: &str) -> String {
    canonical_tf(tf)
        .or_else(|| canonical_tf(&tf.to_lowercase()))
        .unwrap_or(tf)
        .to_string()
}

/// A timeframe made only of digits, as whole minutes (`/^\d+$/` + `parseInt`).
pub fn parse_minutes(tf: &str) -> Option<usize> {
    if tf.is_empty() || !tf.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tf.parse().ok()
}

/// Largest native sub-timeframe (minutes) that evenly divides `target_min`.
pub fn select_sub_tf(native_minutes: &[usize], target_min: usize) -> Option<usize> {
    native_minutes
        .iter()
        .copied()
        .filter(|m| *m < target_min && target_min.is_multiple_of(*m))
        .max()
}

/// Sort by open-time and drop duplicate open-times (incoming wins): the bar contract.
pub fn dedupe_sorted(bars: &[Ohlcv]) -> Vec<Ohlcv> {
    let by_time: BTreeMap<i64, Ohlcv> = bars.iter().map(|b| (b.time, *b)).collect();
    by_time.into_values().collect()
}

/// Fold ascending sub-candles into buckets keyed by `key_ms(time)`; the bucket time is the key.
pub fn aggregate_by(sub: &[Ohlcv], key_ms: impl Fn(i64) -> i64) -> Vec<Ohlcv> {
    let mut buckets: BTreeMap<i64, Ohlcv> = BTreeMap::new();
    for b in sub {
        let key = key_ms(b.time);
        match buckets.get_mut(&key) {
            None => {
                buckets.insert(
                    key,
                    Ohlcv::new(
                        key,
                        b.open,
                        b.high,
                        b.low,
                        b.close,
                        Some(b.volume.unwrap_or(0.0)),
                    ),
                );
            }
            Some(cur) => {
                cur.high = js_max(cur.high, b.high);
                cur.low = js_min(cur.low, b.low);
                cur.close = b.close;
                cur.volume = Some(cur.volume.unwrap_or(0.0) + b.volume.unwrap_or(0.0));
            }
        }
    }
    buckets.into_values().collect()
}

/// Aggregate ascending sub-candles into `bucket_ms` buckets aligned to the epoch.
pub fn aggregate(sub: &[Ohlcv], bucket_ms: i64) -> Vec<Ohlcv> {
    aggregate_by(sub, |t| t.div_euclid(bucket_ms) * bucket_ms)
}

/// Keep the most-recent `limit` bars. A limit of 0 keeps everything, as the
/// original's `slice(-0)` does.
pub fn tail(bars: Vec<Ohlcv>, limit: Option<usize>) -> Vec<Ohlcv> {
    match limit {
        Some(l) if l > 0 && bars.len() > l => bars[bars.len() - l..].to_vec(),
        _ => bars,
    }
}

/// The `SymbolInfo` record all three providers state for a symbol.
pub struct SymbolFields<'a> {
    pub venue: &'a str,
    pub ticker: &'a str,
    pub description: String,
    pub kind: &'a str,
    pub base: Option<Value>,
    pub quote: Option<Value>,
    pub mintick: f64,
}

pub fn build_symbol_info(f: SymbolFields<'_>) -> SymbolInfo {
    let mut extra = Map::new();
    extra.insert(
        "tickerid".into(),
        Value::from(format!("{}:{}", f.venue, f.ticker)),
    );
    extra.insert("prefix".into(), Value::from(f.venue));
    extra.insert("description".into(), Value::from(f.description));
    extra.insert("type".into(), Value::from(f.kind));
    if let Some(base) = f.base {
        extra.insert("basecurrency".into(), base);
    }
    if let Some(quote) = f.quote {
        extra.insert("currency".into(), quote);
    }
    extra.insert("mintick".into(), num_value(f.mintick));
    extra.insert(
        "pricescale".into(),
        num_value(crate::indicators::classics::math::js_round(1.0 / f.mintick)),
    );
    SymbolInfo {
        ticker: f.ticker.to_string(),
        timezone: Some("Etc/UTC".to_string()),
        session: Some("24x7".to_string()),
        extra,
        ..SymbolInfo::default()
    }
}

// ── async building blocks ──

/// A result computed once and shared: the original's `promise ??= start()`.
/// The work starts at the first request and runs on the platform, so it does not
/// depend on any one caller still waiting; later requests share the answer.
/// A failure is cached like a success (the original caches the rejected promise).
pub struct Memo<T: Clone + 'static> {
    state: Rc<RefCell<MemoState<T>>>,
}

enum MemoState<T> {
    Empty,
    Running(Vec<OneshotSender<T>>),
    Done(T),
}

impl<T: Clone + 'static> Clone for Memo<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T: Clone + 'static> Default for Memo<T> {
    fn default() -> Self {
        Self {
            state: Rc::new(RefCell::new(MemoState::Empty)),
        }
    }
}

impl<T: Clone + 'static> Memo<T> {
    /// The shared value; `make` runs only for the first request.
    pub fn get(
        &self,
        platform: &Rc<dyn Platform>,
        make: impl FnOnce() -> LocalBoxFuture<'static, T>,
    ) -> LocalBoxFuture<'static, T> {
        enum Step<T> {
            Ready(T),
            Wait,
            Start,
        }
        let (tx, rx) = oneshot();
        let step = {
            let mut state = self.state.borrow_mut();
            match &mut *state {
                MemoState::Done(value) => Step::Ready(value.clone()),
                MemoState::Running(waiters) => {
                    waiters.push(tx);
                    Step::Wait
                }
                MemoState::Empty => {
                    *state = MemoState::Running(vec![tx]);
                    Step::Start
                }
            }
        };
        match step {
            Step::Ready(value) => Box::pin(async move { value }),
            Step::Wait => Box::pin(rx),
            Step::Start => {
                let work = make();
                let state = self.state.clone();
                platform.spawn(Box::pin(async move {
                    let value = work.await;
                    let waiters = match std::mem::replace(
                        &mut *state.borrow_mut(),
                        MemoState::Done(value.clone()),
                    ) {
                        MemoState::Running(waiters) => waiters,
                        _ => Vec::new(),
                    };
                    for waiter in waiters {
                        waiter.send(value.clone());
                    }
                }));
                Box::pin(rx)
            }
        }
    }
}

/// `Promise.all` over futures of one type: all run concurrently, results in order.
pub async fn join_all<'a, T>(futures: Vec<LocalBoxFuture<'a, T>>) -> Vec<T> {
    let mut slots: Vec<Option<LocalBoxFuture<'a, T>>> = futures.into_iter().map(Some).collect();
    let mut out: Vec<Option<T>> = slots.iter().map(|_| None).collect();
    poll_fn(|cx| {
        let mut pending = false;
        for (slot, result) in slots.iter_mut().zip(out.iter_mut()) {
            if let Some(future) = slot.as_mut() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => {
                        *result = Some(value);
                        *slot = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

/// `Promise.all` over two futures of different types.
pub async fn join2<A, B>(a: impl Future<Output = A>, b: impl Future<Output = B>) -> (A, B) {
    let mut a = Box::pin(a);
    let mut b = Box::pin(b);
    let mut out_a = None;
    let mut out_b = None;
    poll_fn(|cx| {
        if out_a.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            out_a = Some(v);
        }
        if out_b.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            out_b = Some(v);
        }
        if out_a.is_some()
            && out_b.is_some()
            && let (Some(a), Some(b)) = (out_a.take(), out_b.take())
        {
            return Poll::Ready((a, b));
        }
        Poll::Pending
    })
    .await
}

/// A [`BarSink`] shared by a stream and its poll fallback. The callback is taken
/// out of the cell while it runs, so it may call back into the subscription.
#[derive(Clone)]
pub struct SharedSink {
    sink: Rc<RefCell<Option<BarSink>>>,
}

impl SharedSink {
    pub fn new(sink: BarSink) -> Self {
        Self {
            sink: Rc::new(RefCell::new(Some(sink))),
        }
    }

    pub fn emit(&self, bar: Ohlcv) {
        let taken = self.sink.borrow_mut().take();
        if let Some(mut sink) = taken {
            sink(bar);
            let mut slot = self.sink.borrow_mut();
            if slot.is_none() {
                *slot = Some(sink);
            }
        }
    }
}

struct Poller {
    platform: Rc<dyn Platform>,
    get: Box<dyn Fn() -> LocalBoxFuture<'static, Vec<Ohlcv>>>,
    on_bar: SharedSink,
    stopped: Cell<bool>,
    timer: Cell<Option<TimerId>>,
}

impl Poller {
    fn schedule(self: &Rc<Self>) {
        let poller = self.clone();
        let id = self.platform.set_timeout(
            POLL_MS,
            Box::new(move || {
                let task = poller.clone();
                poller.platform.spawn(Box::pin(task.poll()));
            }),
        );
        self.timer.set(Some(id));
    }

    async fn poll(self: Rc<Self>) {
        if self.stopped.get() {
            return;
        }
        let bars = (self.get)().await;
        for bar in bars {
            self.on_bar.emit(bar);
        }
        if !self.stopped.get() {
            self.schedule();
        }
    }
}

/// Poll the forming candle every 3 s (timeframes without a native stream, and the
/// fallback of a stream that stays silent). `get` is the provider's own fail-soft
/// `getBars` for the last two bars. A read already in flight when the returned
/// handle is called still delivers its bars, as in the original; no further poll starts.
pub fn poll_bars(
    platform: Rc<dyn Platform>,
    get: impl Fn() -> LocalBoxFuture<'static, Vec<Ohlcv>> + 'static,
    on_bar: SharedSink,
) -> Unsubscribe {
    let poller = Rc::new(Poller {
        platform,
        get: Box::new(get),
        on_bar,
        stopped: Cell::new(false),
        timer: Cell::new(None),
    });
    poller.schedule();
    Box::new(move || {
        poller.stopped.set(true);
        if let Some(id) = poller.timer.take() {
            poller.platform.clear_timeout(id);
        }
    })
}

struct IntervalState {
    platform: Rc<dyn Platform>,
    ms: f64,
    tick: RefCell<Option<Rc<dyn Fn()>>>,
    timer: Cell<Option<TimerId>>,
}

/// `setInterval`, built on the platform's one-shot timers.
pub struct Interval {
    state: Rc<IntervalState>,
}

impl Interval {
    pub fn start(platform: &Rc<dyn Platform>, ms: f64, tick: impl Fn() + 'static) -> Self {
        let state = Rc::new(IntervalState {
            platform: platform.clone(),
            ms,
            tick: RefCell::new(Some(Rc::new(tick))),
            timer: Cell::new(None),
        });
        Self::arm(&state);
        Self { state }
    }

    fn arm(state: &Rc<IntervalState>) {
        let this = state.clone();
        let id = state.platform.set_timeout(
            state.ms,
            Box::new(move || {
                let tick = this.tick.borrow().clone();
                if let Some(tick) = tick {
                    Interval::arm(&this);
                    tick();
                }
            }),
        );
        state.timer.set(Some(id));
    }

    /// `clearInterval`.
    pub fn clear(self) {
        self.state.tick.borrow_mut().take();
        if let Some(id) = self.state.timer.take() {
            self.state.platform.clear_timeout(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    #[test]
    fn number_coercions_follow_javascript() {
        assert_eq!(js_str_to_num(" 12.5 "), 12.5);
        assert_eq!(js_str_to_num(""), 0.0);
        assert!(js_str_to_num("abc").is_nan());
        assert!(js_str_to_num("inf").is_nan());
        assert_eq!(js_str_to_num("0x10"), 16.0);
        assert_eq!(js_num(Some(&json!("5"))), 5.0);
        assert_eq!(js_num(Some(&Value::Null)), 0.0);
        assert!(js_num(None).is_nan());
        assert_eq!(js_parse_float("0.001"), 0.001);
        assert_eq!(js_parse_float("1e-3abc"), 0.001);
        assert_eq!(js_parse_float("  -2.5x"), -2.5);
        assert_eq!(js_parse_float(".5"), 0.5);
        assert!(js_parse_float("abc").is_nan());
        assert!(js_parse_float("").is_nan());
        assert_eq!(js_text(Some(&json!(3))), "3");
        assert_eq!(js_text(None), "undefined");
        assert_eq!(num_value(100.0), json!(100));
        assert_eq!(num_value(0.1), json!(0.1));
        assert_eq!(num_value(f64::INFINITY), Value::Null);
    }

    #[test]
    fn query_values_are_escaped_like_url_search_params() {
        assert_eq!(encode_query_value("BTC-USDT_1.x*"), "BTC-USDT_1.x*");
        assert_eq!(encode_query_value("a b&c"), "a+b%26c");
    }

    #[test]
    fn timeframes_normalize_and_pick_a_sub_timeframe() {
        assert_eq!(normalize_tf("1H"), "60");
        assert_eq!(normalize_tf("1mo"), "M");
        assert_eq!(normalize_tf("MAX"), "MAX");
        assert_eq!(parse_minutes("45"), Some(45));
        assert_eq!(parse_minutes("4h"), None);
        assert_eq!(select_sub_tf(&[1, 3, 5, 15, 30, 60], 45), Some(15));
        assert_eq!(select_sub_tf(&[1, 3, 5, 15, 30, 60], 1), None);
    }

    #[test]
    fn tail_keeps_everything_for_a_zero_limit() {
        let bars: Vec<Ohlcv> = (0..4)
            .map(|t| Ohlcv::new(t, 1.0, 1.0, 1.0, 1.0, None))
            .collect();
        assert_eq!(tail(bars.clone(), Some(2)).len(), 2);
        assert_eq!(tail(bars.clone(), Some(0)).len(), 4);
        assert_eq!(tail(bars, None).len(), 4);
    }

    #[test]
    fn memo_runs_its_work_once_and_shares_the_answer() {
        let platform = ManualPlatform::default();
        let handle = platform.handle();
        let runs = Rc::new(Cell::new(0));
        let memo: Memo<i32> = Memo::default();
        let make = |runs: Rc<Cell<i32>>, p: Rc<dyn Platform>| -> LocalBoxFuture<'static, i32> {
            Box::pin(async move {
                runs.set(runs.get() + 1);
                crate::runtime::sleep(&*p, 10.0).await;
                7
            })
        };
        let first = memo.get(&handle, || make(runs.clone(), handle.clone()));
        let second = memo.get(&handle, || make(runs.clone(), handle.clone()));
        let got = platform.block_on(async move { (first.await, second.await) });
        assert_eq!(got, (7, 7));
        let third = memo.get(&handle, || make(runs.clone(), handle.clone()));
        assert_eq!(platform.block_on(third), 7);
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn joins_run_concurrently() {
        let platform = ManualPlatform::default();
        let handle = platform.handle();
        let sleeper = |ms: f64, v: i32| -> LocalBoxFuture<'static, i32> {
            let p = handle.clone();
            Box::pin(async move {
                crate::runtime::sleep(&*p, ms).await;
                v
            })
        };
        let all = join_all(vec![sleeper(30.0, 1), sleeper(10.0, 2), sleeper(20.0, 3)]);
        assert_eq!(platform.block_on(all), vec![1, 2, 3]);
        assert_eq!(platform.now_ms(), 30.0);
        let pair = join2(sleeper(5.0, 1), async { "x" });
        assert_eq!(platform.block_on(pair), (1, "x"));
    }

    #[test]
    fn an_interval_repeats_until_cleared() {
        let platform = ManualPlatform::default();
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        let interval = Interval::start(&platform.handle(), 100.0, move || c.set(c.get() + 1));
        platform.advance(350.0);
        assert_eq!(count.get(), 3);
        interval.clear();
        platform.advance(1000.0);
        assert_eq!(count.get(), 3);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn dedupe_keeps_the_incoming_bar_and_sorts() {
        let bar = |time, open| Ohlcv::new(time, open, open, open, open, Some(0.0));
        let out = dedupe_sorted(&[bar(2, 1.0), bar(1, 1.0), bar(1, 9.0)]);
        assert_eq!(out.iter().map(|b| b.time).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(out[0].open, 9.0);
    }
}
