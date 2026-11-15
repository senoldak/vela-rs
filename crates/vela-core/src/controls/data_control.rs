//! The chart's data control surface (`chart.data`): registers market-data
//! providers and queries the registry (resolve a symbol, list providers and
//! symbols, fetch per-symbol metadata).
//!
//! It operates on the default [`MultiProviderFeed`]. A chart built on a fully
//! custom feed manages its own data: the registration and query methods then
//! warn (through the platform) and do nothing, mirroring `chart.renderer.set`
//! on an unsupported feature.
//!
//! The original recognizes the default feed with `instanceof`. A trait object
//! cannot be downcast, so the caller states it: [`DataControl::new`] takes the
//! default feed as `Some(feed)`, or `None` for a custom one.

use std::rc::Rc;

use crate::data::multi_provider_feed::MultiProviderFeed;
use crate::data::provider_registry::Resolved;
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::SymbolInfo;
use crate::runtime::{DataError, LocalBoxFuture, Platform};

/// `chart.data`. A cheap handle: clones share the one registry.
#[derive(Clone)]
pub struct DataControl {
    registry: Option<MultiProviderFeed>,
    platform: Option<Rc<dyn Platform>>,
}

impl DataControl {
    /// `registry` is the chart's default feed, or `None` when a custom feed was
    /// injected. `platform` spawns the background index build of a registered
    /// provider and receives the warnings.
    pub fn new(registry: Option<MultiProviderFeed>, platform: Rc<dyn Platform>) -> Self {
        Self {
            registry,
            platform: Some(platform),
        }
    }

    /// A control with no registry and no platform: every query is empty and
    /// registrations are dropped silently. What a native indicator's context
    /// answers with when its host gives it no market-data access.
    pub fn detached() -> Self {
        Self {
            registry: None,
            platform: None,
        }
    }

    /// Whether the chart runs on the default provider registry.
    pub fn has_registry(&self) -> bool {
        self.registry.is_some()
    }

    /// Register (or replace) a data provider under `name`. Any symbol prefixed
    /// `name:` (case-insensitive), or a bare symbol the provider's index
    /// contains, routes to it. Returns at once and is chainable: it kicks a
    /// background symbol-index build, and if the chart symbol resolves through
    /// this provider the parked initial load fires. Await the chart's `ready`
    /// for that load, or [`ready`](Self::ready) for the index.
    pub fn register_provider(&self, name: &str, provider: Rc<dyn DataProvider>) -> &Self {
        match (&self.registry, &self.platform) {
            (Some(registry), Some(platform)) => {
                platform.spawn(registry.register_provider(name, provider));
            }
            _ => {
                if let Some(platform) = &self.platform {
                    platform.warn(&format!(
                        "[vela] chart.data.registerProvider(\"{name}\") ignored \u{2014} the active data feed \
                         is a custom feed (deps.dataFeed), not the default provider registry."
                    ));
                }
            }
        }
        self
    }

    /// Remove a registered provider.
    pub fn unregister_provider(&self, name: &str) -> &Self {
        if let Some(registry) = &self.registry {
            registry.unregister_provider(name);
        }
        self
    }

    /// Metadata for every registered provider.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.registry
            .as_ref()
            .map(MultiProviderFeed::providers)
            .unwrap_or_default()
    }

    /// Resolve a symbol string to `{ provider, ticker }`, or `None` if nothing serves it.
    pub fn resolve(&self, symbol: &str) -> Option<Resolved> {
        self.registry.as_ref()?.resolve_symbol(symbol)
    }

    /// The DISPLAY prefix for `symbol`: the listing venue its descriptor
    /// declares (`NASDAQ` for AAPL) or the resolved provider name. `None`
    /// while unresolvable.
    pub fn display_prefix(&self, symbol: &str) -> Option<String> {
        self.registry.as_ref()?.display_prefix(symbol)
    }

    /// The canonical `PREFIX:TICKER` form of `symbol`, or `None` while unresolvable.
    pub fn canonical_symbol(&self, symbol: &str) -> Option<String> {
        self.registry.as_ref()?.canonical_symbol(symbol)
    }

    /// The registered provider INSTANCE under `name`: the seam for EXTENDED
    /// provider surfaces (a provider may implement interfaces beyond the
    /// [`DataProvider`] port). `None` if the name is unknown or a custom feed
    /// bypasses the registry.
    pub fn provider_instance(&self, name: &str) -> Option<Rc<dyn DataProvider>> {
        self.registry.as_ref()?.provider_instance(name)
    }

    /// Indexed symbols for one provider (or all): for autocomplete.
    pub fn symbols(&self, provider: Option<&str>) -> Vec<SymbolDescriptor> {
        self.registry
            .as_ref()
            .map(|registry| registry.symbols(provider))
            .unwrap_or_default()
    }

    /// The icon URL for `symbol`: its owning provider's `resolve_symbol_icon`,
    /// routed through resolution. `None` while unresolvable, when the provider
    /// declares no resolver, or on a custom feed; the shells then show initials.
    pub fn symbol_icon(&self, symbol: &str) -> Option<String> {
        self.registry.as_ref()?.symbol_icon(symbol)
    }

    /// Per-symbol metadata (Pine `syminfo.*`), resolved through the owning provider.
    pub fn symbol_info(
        &self,
        symbol: &str,
    ) -> LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>> {
        match &self.registry {
            Some(registry) => registry.symbol_info_for(symbol),
            None => Box::pin(std::future::ready(Ok(None))),
        }
    }

    /// The full resolved capabilities for `symbol`: per-instrument when the
    /// provider refines them (`capabilities_for`), else its provider-wide
    /// declaration. `None` while nothing resolves the symbol (a provider may
    /// still be registering), so callers that can act later should re-read
    /// rather than latch the first answer.
    pub fn capabilities(&self, symbol: &str) -> Option<ProviderCapabilities> {
        self.registry.as_ref()?.capabilities_for(symbol)
    }

    /// Resolves when every registered provider's eager index has settled.
    pub fn ready(&self) -> LocalBoxFuture<'static, ()> {
        match &self.registry {
            Some(registry) => registry.ready(),
            None => Box::pin(std::future::ready(())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::data::BarStore;
    use crate::data::provider_registry::test_support::FakeProvider;
    use crate::runtime::testing::ManualPlatform;

    fn rig() -> (ManualPlatform, DataControl) {
        let platform = ManualPlatform::new(0.0);
        let store = Rc::new(RefCell::new(BarStore::new()));
        let feed = MultiProviderFeed::new(store, platform.handle());
        let control = DataControl::new(Some(feed), platform.handle());
        (platform, control)
    }

    fn custom_feed() -> (ManualPlatform, DataControl) {
        let platform = ManualPlatform::new(0.0);
        let control = DataControl::new(None, platform.handle());
        (platform, control)
    }

    #[test]
    fn registering_a_provider_makes_its_symbols_resolve() {
        let (platform, data) = rig();
        assert!(data.has_registry());
        assert!(data.resolve("BTCUSDT").is_none());
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])))
            .register_provider("other", Rc::new(FakeProvider::new(&["ETHUSDT"])));
        platform.run();
        assert_eq!(
            data.providers()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["fake", "other"]
        );
        let resolved = data.resolve("BTCUSDT").expect("resolved");
        assert_eq!(
            (resolved.provider.as_str(), resolved.ticker.as_str()),
            ("fake", "BTCUSDT")
        );
        let pinned = data.resolve("OTHER:ETHUSDT").expect("pinned");
        assert_eq!(pinned.provider, "other");
        assert_eq!(
            data.canonical_symbol("BTCUSDT").as_deref(),
            Some("fake:BTCUSDT")
        );
        assert_eq!(data.display_prefix("BTCUSDT").as_deref(), Some("fake"));
        assert!(data.provider_instance("fake").is_some());
        assert!(data.provider_instance("nope").is_none());
        assert_eq!(data.symbols(Some("other")).len(), 1);
        assert_eq!(data.symbols(None).len(), 2);
        assert!(data.capabilities("BTCUSDT").is_some());
        assert!(data.capabilities("NOPE").is_none());
        assert!(platform.warnings().is_empty());
    }

    #[test]
    fn unregistering_forgets_the_provider() {
        let (platform, data) = rig();
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])));
        platform.run();
        assert!(data.resolve("BTCUSDT").is_some());
        data.unregister_provider("fake");
        assert!(data.resolve("BTCUSDT").is_none());
        assert!(data.providers().is_empty());
    }

    #[test]
    fn ready_settles_once_the_index_is_built() {
        let (platform, data) = rig();
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])));
        platform.block_on(data.ready());
        assert_eq!(data.symbols(None).len(), 1);
    }

    #[test]
    fn symbol_info_resolves_through_the_owning_provider() {
        let (platform, data) = rig();
        data.register_provider(
            "fake",
            Rc::new(
                FakeProvider::new(&["AAPL"]).sym_info(
                    serde_json::json!({ "ticker": "AAPL", "timezone": "America/New_York" }),
                ),
            ),
        );
        platform.run();
        let info = platform
            .block_on(data.symbol_info("fake:AAPL"))
            .expect("no error")
            .expect("info");
        assert_eq!(info.ticker, "AAPL");
        assert_eq!(
            info.extra.get("timezone"),
            Some(&serde_json::json!("America/New_York"))
        );
        assert_eq!(platform.block_on(data.symbol_info("ZZZ")), Ok(None));
    }

    #[test]
    fn a_custom_feed_warns_on_registration_and_answers_empty() {
        let (platform, data) = custom_feed();
        assert!(!data.has_registry());
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])));
        platform.run();
        assert_eq!(
            platform.warnings(),
            [
                "[vela] chart.data.registerProvider(\"fake\") ignored \u{2014} the active data feed is a custom feed (deps.dataFeed), not the default provider registry."
            ]
        );
        data.unregister_provider("fake");
        assert!(data.providers().is_empty());
        assert!(data.resolve("BTCUSDT").is_none());
        assert!(data.display_prefix("BTCUSDT").is_none());
        assert!(data.canonical_symbol("BTCUSDT").is_none());
        assert!(data.provider_instance("fake").is_none());
        assert!(data.symbols(None).is_empty());
        assert!(data.symbol_icon("BTCUSDT").is_none());
        assert!(data.capabilities("BTCUSDT").is_none());
        assert_eq!(platform.block_on(data.symbol_info("BTCUSDT")), Ok(None));
        platform.block_on(data.ready());
        assert_eq!(platform.warnings().len(), 1);
    }

    #[test]
    fn a_detached_control_is_silent() {
        let data = DataControl::detached();
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])));
        assert!(data.providers().is_empty());
        assert!(data.resolve("BTCUSDT").is_none());
    }

    #[test]
    fn clones_share_one_registry() {
        let (platform, data) = rig();
        let twin = data.clone();
        data.register_provider("fake", Rc::new(FakeProvider::new(&["BTCUSDT"])));
        platform.run();
        assert!(twin.resolve("BTCUSDT").is_some());
    }
}
