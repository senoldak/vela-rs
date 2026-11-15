//! Owns the live tick subscription when `live: true`. Pulls forming-candle ticks
//! from the market-data feed (Vela owns the data) and forwards them to the
//! orchestrator's bar reconciler (`LiveSession.ts`).

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::Ohlcv;
use crate::ports::market_data::{MarketConfig, MarketDataFeed};
use crate::runtime::Unsubscribe;

/// The live subscription of one chart market.
pub struct LiveSession {
    feed: Rc<dyn MarketDataFeed>,
    market: MarketConfig,
    on_bar: Rc<dyn Fn(Ohlcv)>,
    unsubscribe: RefCell<Option<Unsubscribe>>,
}

impl LiveSession {
    /// `on_bar` receives every forming-candle tick. Hold a `Weak` of the
    /// orchestrator in it, not an `Rc`: the session lives inside the
    /// orchestrator.
    pub fn new(
        feed: Rc<dyn MarketDataFeed>,
        market: MarketConfig,
        on_bar: Rc<dyn Fn(Ohlcv)>,
    ) -> Self {
        Self {
            feed,
            market,
            on_bar,
            unsubscribe: RefCell::new(None),
        }
    }

    /// Subscribe. Does nothing while already subscribed.
    pub fn start(&self) {
        if self.unsubscribe.borrow().is_some() {
            return;
        }
        let on_bar = Rc::clone(&self.on_bar);
        let unsubscribe = self
            .feed
            .subscribe(&self.market, Box::new(move |bar| on_bar(bar)));
        *self.unsubscribe.borrow_mut() = Some(unsubscribe);
    }

    /// Unsubscribe. Does nothing while not subscribed; a later `start`
    /// subscribes again.
    pub fn stop(&self) {
        let unsubscribe = self.unsubscribe.borrow_mut().take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::runtime::{DataError, LocalBoxFuture};

    #[derive(Default)]
    struct FakeFeed {
        subscribed: RefCell<Vec<(Option<String>, crate::ports::market_data::BarSink)>>,
        live: Rc<Cell<usize>>,
    }

    impl MarketDataFeed for FakeFeed {
        fn load(
            &self,
            _cfg: &MarketConfig,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }

        fn subscribe(
            &self,
            cfg: &MarketConfig,
            on_bar: crate::ports::market_data::BarSink,
        ) -> Unsubscribe {
            self.subscribed
                .borrow_mut()
                .push((cfg.symbol.clone(), on_bar));
            self.live.set(self.live.get() + 1);
            let live = self.live.clone();
            Box::new(move || live.set(live.get() - 1))
        }
    }

    fn rig() -> (Rc<FakeFeed>, LiveSession, Rc<RefCell<Vec<Ohlcv>>>) {
        let feed = Rc::new(FakeFeed::default());
        let seen: Rc<RefCell<Vec<Ohlcv>>> = Rc::default();
        let sink = seen.clone();
        let session = LiveSession::new(
            feed.clone(),
            MarketConfig {
                symbol: Some("BTCUSDT".into()),
                ..MarketConfig::default()
            },
            Rc::new(move |bar| sink.borrow_mut().push(bar)),
        );
        (feed, session, seen)
    }

    fn bar(time: i64) -> Ohlcv {
        Ohlcv::new(time, 1.0, 2.0, 0.5, 1.5, Some(3.0))
    }

    #[test]
    fn start_subscribes_with_the_market_and_forwards_ticks() {
        let (feed, session, seen) = rig();
        session.start();
        assert_eq!(feed.live.get(), 1);
        {
            let mut subscribed = feed.subscribed.borrow_mut();
            assert_eq!(subscribed[0].0.as_deref(), Some("BTCUSDT"));
            (subscribed[0].1)(bar(1));
            (subscribed[0].1)(bar(2));
        }
        assert_eq!(
            seen.borrow().iter().map(|b| b.time).collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn starting_twice_subscribes_once() {
        let (feed, session, _) = rig();
        session.start();
        session.start();
        assert_eq!(feed.subscribed.borrow().len(), 1);
        assert_eq!(feed.live.get(), 1);
    }

    #[test]
    fn stop_unsubscribes_and_a_later_start_resubscribes() {
        let (feed, session, seen) = rig();
        session.stop();
        assert_eq!(feed.live.get(), 0);
        session.start();
        session.stop();
        assert_eq!(feed.live.get(), 0);
        session.stop();
        session.start();
        assert_eq!(feed.subscribed.borrow().len(), 2);
        assert_eq!(feed.live.get(), 1);
        (feed.subscribed.borrow_mut()[1].1)(bar(7));
        assert_eq!(seen.borrow().len(), 1);
    }
}
