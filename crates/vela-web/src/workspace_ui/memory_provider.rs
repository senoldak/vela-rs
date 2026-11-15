//! The default data source of a browser workspace: bars held in memory, seeded from
//! JavaScript. No network.
//!
//! [`MemoryProvider`] is a [`DataProvider`] registered on the workspace's shared feed under
//! one name (`memory` by default). It serves the series it was given per symbol and
//! timeframe, enumerates its symbols (so a bare `BTC` resolves to it and a picker can list
//! them), streams ticks through [`MemoryProvider::tick`], and keeps the ticked series so a
//! later reload (a timeframe round trip, a layout change) shows them.
//!
//! A series is looked up by the timeframe as written (`"60"`), then by the timeframe's
//! duration (`"60"` and `"1h"` are the same series), then the symbol's catch-all series
//! (given as a bare array), then its first series: a demo asking a timeframe it has no
//! bars for still gets a chart instead of an empty pane.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vela_core::data::timeframe_to_ms;
use vela_core::model::Ohlcv;
use vela_core::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use vela_core::ports::market_data::{BarRange, BarSink};
use vela_core::runtime::{DataError, LocalBoxFuture, Unsubscribe};

/// One symbol's bars: per timeframe, plus an optional catch-all.
#[derive(Clone, Default)]
pub struct SymbolSeries {
    by_timeframe: Vec<(String, Vec<Ohlcv>)>,
    any: Option<Vec<Ohlcv>>,
}

impl SymbolSeries {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve `bars` for `timeframe`.
    pub fn with_timeframe(mut self, timeframe: &str, bars: Vec<Ohlcv>) -> Self {
        self.by_timeframe.push((timeframe.to_string(), bars));
        self
    }

    /// Serve `bars` for any timeframe without a series of its own.
    pub fn with_any(mut self, bars: Vec<Ohlcv>) -> Self {
        self.any = Some(bars);
        self
    }

    fn index_of(&self, timeframe: &str) -> Option<usize> {
        if let Some(i) = self.by_timeframe.iter().position(|(k, _)| k == timeframe) {
            return Some(i);
        }
        let ms = timeframe_to_ms(timeframe);
        if ms > 0 {
            return self
                .by_timeframe
                .iter()
                .position(|(k, _)| timeframe_to_ms(k) == ms);
        }
        None
    }

    fn bars_for(&self, timeframe: &str) -> Vec<Ohlcv> {
        match self.index_of(timeframe) {
            Some(i) => self.by_timeframe[i].1.clone(),
            None => self
                .any
                .clone()
                .or_else(|| self.by_timeframe.first().map(|(_, b)| b.clone()))
                .unwrap_or_default(),
        }
    }

    fn push_tick(&mut self, timeframe: &str, bar: &Ohlcv) {
        let target = match self.index_of(timeframe) {
            Some(i) => Some(&mut self.by_timeframe[i].1),
            None => self.any.as_mut(),
        };
        let Some(list) = target else { return };
        match list.last_mut() {
            Some(last) if last.time == bar.time => *last = *bar,
            Some(last) if last.time > bar.time => {}
            _ => list.push(*bar),
        }
    }
}

type SharedSink = Rc<RefCell<BarSink>>;

struct Subscription {
    id: u64,
    ticker: String,
    timeframe_ms: i64,
    timeframe: String,
    sink: SharedSink,
}

/// The in-memory provider. See the module documentation.
pub struct MemoryProvider {
    name: String,
    /// Series keyed by the upper-cased ticker, in insertion order for the symbol list.
    series: RefCell<Vec<(String, SymbolSeries)>>,
    /// What the symbol list says about a ticker beyond its name (description, instrument
    /// class, listing venue), by upper-cased ticker.
    info: RefCell<Vec<(String, SymbolDescriptor)>>,
    subs: Rc<RefCell<Vec<Subscription>>>,
    next: Cell<u64>,
}

fn key_of(ticker: &str) -> String {
    ticker.trim().to_uppercase()
}

impl MemoryProvider {
    pub fn new(name: &str) -> Rc<Self> {
        Rc::new(Self {
            name: name.to_string(),
            series: RefCell::default(),
            info: RefCell::default(),
            subs: Rc::default(),
            next: Cell::new(1),
        })
    }

    /// The registration name (the feed lower-cases it).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Add or replace one symbol's series.
    pub fn set_symbol(&self, ticker: &str, series: SymbolSeries) {
        let key = key_of(ticker);
        let mut all = self.series.borrow_mut();
        match all.iter_mut().find(|(k, _)| *k == key) {
            Some((_, s)) => *s = series,
            None => all.push((key, series)),
        }
    }

    /// What the symbol list reports for `ticker` besides its name: `description`, `kind` (the
    /// instrument class) and `prefix` (the listing venue) of `info` (its `ticker` is ignored).
    pub fn set_info(&self, ticker: &str, info: SymbolDescriptor) {
        let key = key_of(ticker);
        let mut all = self.info.borrow_mut();
        match all.iter_mut().find(|(k, _)| *k == key) {
            Some((_, d)) => *d = info,
            None => all.push((key, info)),
        }
    }

    /// The symbol list: every symbol served, in the order given, with the info set for it.
    pub fn descriptors(&self) -> Vec<SymbolDescriptor> {
        let info = self.info.borrow();
        self.tickers()
            .into_iter()
            .map(|ticker| {
                let mut d = info
                    .iter()
                    .find(|(k, _)| *k == ticker)
                    .map(|(_, d)| d.clone())
                    .unwrap_or_default();
                d.ticker = ticker;
                d
            })
            .collect()
    }

    /// The symbols served, in the order they were given.
    pub fn tickers(&self) -> Vec<String> {
        self.series
            .borrow()
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// The bars currently held for a symbol and timeframe (empty for an unknown symbol).
    pub fn bars(&self, ticker: &str, timeframe: &str) -> Vec<Ohlcv> {
        let key = key_of(ticker);
        self.series
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.bars_for(timeframe))
            .unwrap_or_default()
    }

    /// Deliver one live bar for `ticker` at `timeframe` to every subscribed chart (a bar
    /// with the last bar's time replaces it, a newer one opens the next), and keep it in
    /// the series. Returns how many subscribers received it.
    pub fn tick(&self, ticker: &str, timeframe: &str, bar: Ohlcv) -> usize {
        let key = key_of(ticker);
        {
            let mut all = self.series.borrow_mut();
            if let Some((_, s)) = all.iter_mut().find(|(k, _)| *k == key) {
                s.push_tick(timeframe, &bar);
            }
        }
        let ms = timeframe_to_ms(timeframe);
        // A snapshot: a sink may subscribe or unsubscribe from inside its call.
        let sinks: Vec<SharedSink> = self
            .subs
            .borrow()
            .iter()
            .filter(|s| {
                s.ticker == key && (s.timeframe == timeframe || (ms > 0 && s.timeframe_ms == ms))
            })
            .map(|s| s.sink.clone())
            .collect();
        let mut delivered = 0;
        for sink in sinks {
            if let Ok(mut f) = sink.try_borrow_mut() {
                f(bar);
                delivered += 1;
            }
        }
        delivered
    }
}

impl DataProvider for MemoryProvider {
    fn get_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let mut bars = self.bars(ticker, timeframe);
        if let Some(to) = range.to {
            bars.retain(|b| b.time <= to);
        }
        if let Some(limit) = range.limit
            && bars.len() > limit
        {
            bars.drain(..bars.len() - limit);
        }
        Box::pin(std::future::ready(Ok(bars)))
    }

    fn info(&self) -> Option<ProviderInfo> {
        Some(ProviderInfo {
            name: self.name.to_lowercase(),
            display_name: Some("Memory".to_string()),
            requires_api_key: None,
            supported_timeframes: None,
            capabilities: ProviderCapabilities {
                enumerate: true,
                stream: true,
                symbol_info: false,
            },
        })
    }

    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        Some(Box::pin(std::future::ready(Ok(self.descriptors()))))
    }

    fn subscribe(
        &self,
        ticker: &str,
        timeframe: &str,
        on_bar: BarSink,
        _session: Option<&str>,
    ) -> Option<Unsubscribe> {
        let id = self.next.get();
        self.next.set(id + 1);
        self.subs.borrow_mut().push(Subscription {
            id,
            ticker: key_of(ticker),
            timeframe_ms: timeframe_to_ms(timeframe),
            timeframe: timeframe.to_string(),
            sink: Rc::new(RefCell::new(on_bar)),
        });
        let subs = Rc::downgrade(&self.subs);
        Some(Box::new(move || {
            if let Some(subs) = subs.upgrade() {
                subs.borrow_mut().retain(|s| s.id != id);
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: i64, c: f64) -> Ohlcv {
        Ohlcv::new(t, c, c + 1.0, c - 1.0, c, Some(1.0))
    }

    #[test]
    fn a_series_is_found_by_text_by_duration_then_by_the_catch_all() {
        let p = MemoryProvider::new("memory");
        p.set_symbol(
            "btc",
            SymbolSeries::new()
                .with_timeframe("60", vec![bar(1, 1.0), bar(2, 2.0)])
                .with_timeframe("1D", vec![bar(9, 9.0)]),
        );
        assert_eq!(p.bars("BTC", "60").len(), 2);
        assert_eq!(p.bars("btc", "1h").len(), 2);
        assert_eq!(p.bars("BTC", "D").len(), 1);
        // No series for 5m and no catch-all: the first series stands in.
        assert_eq!(p.bars("BTC", "5").len(), 2);
        assert!(p.bars("ETH", "60").is_empty());
        assert_eq!(p.tickers(), ["BTC"]);
    }

    #[test]
    fn the_symbol_list_carries_the_info_set_for_each_ticker() {
        let p = MemoryProvider::new("memory");
        p.set_symbol("a", SymbolSeries::new().with_any(vec![bar(1, 1.0)]));
        p.set_symbol("b", SymbolSeries::new().with_any(vec![bar(1, 1.0)]));
        let mut info = SymbolDescriptor::new("ignored");
        info.description = Some("Alpha Inc".into());
        info.kind = Some("stock".into());
        info.prefix = Some("NORTH".into());
        p.set_info("A", info);
        let list = p.descriptors();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].ticker, "A");
        assert_eq!(list[0].description.as_deref(), Some("Alpha Inc"));
        assert_eq!(list[0].kind.as_deref(), Some("stock"));
        assert_eq!(list[0].prefix.as_deref(), Some("NORTH"));
        assert_eq!(
            list[1],
            SymbolDescriptor::new("B"),
            "no info: the bare ticker"
        );
    }

    #[test]
    fn a_tick_replaces_or_opens_a_bar_and_reaches_the_subscriber() {
        let p = MemoryProvider::new("memory");
        p.set_symbol("A", SymbolSeries::new().with_any(vec![bar(1, 1.0)]));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        let off = p
            .subscribe(
                "A",
                "60",
                Box::new(move |b| sink.borrow_mut().push(b.time)),
                None,
            )
            .unwrap();
        assert_eq!(p.tick("a", "60", bar(1, 5.0)), 1);
        assert_eq!(p.tick("a", "60", bar(2, 6.0)), 1);
        assert_eq!(*seen.borrow(), [1, 2]);
        assert_eq!(p.bars("A", "60").len(), 2);
        off();
        assert_eq!(p.tick("a", "60", bar(3, 7.0)), 0);
    }
}
