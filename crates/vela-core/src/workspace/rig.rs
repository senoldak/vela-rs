//! The shared test rig of the stateful workspace tests: a manual platform, a fake data
//! provider, the in-memory workspace host and a ready-made workspace.

use std::cell::RefCell;
use std::rc::Rc;

use crate::data::timeframe_to_ms;
use crate::data_unavailable::{
    DataUnavailableDetails, DataUnavailableError, DataUnavailableReason,
};
use crate::model::Ohlcv;
use crate::options::VelaOptions;
use crate::ports::data_provider::{DataProvider, SymbolDescriptor};
use crate::ports::market_data::{BarRange, SymbolInfo};
use crate::runtime::testing::ManualPlatform;
use crate::runtime::{DataError, LocalBoxFuture};
use crate::vela::VelaEnv;

use super::cell::CellSeed;
use super::layouts::LayoutRegistry;
use super::shell::{LayoutChoice, Workspace, WorkspaceOptions};
use super::testing::TestWorkspaceHost;

pub const T0: i64 = 1_700_000_000_000;
pub const SYMBOLS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "EEE"];

/// Serves 300 deterministic bars per request: the series depends on the ticker (its first
/// open) and the timeframe (the spacing), so two cells on different symbols show different
/// data.
pub struct FakeProvider {
    pub calls: RefCell<Vec<(String, String)>>,
    pub info: RefCell<Option<SymbolInfo>>,
    /// A timeframe the source refuses for good, with the one it suggests instead.
    pub refuse: RefCell<Option<(String, String)>>,
}

impl FakeProvider {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            calls: RefCell::default(),
            info: RefCell::default(),
            refuse: RefCell::default(),
        })
    }

    pub fn bars_for(ticker: &str, timeframe: &str) -> Vec<Ohlcv> {
        let base: f64 = 50.0 + ticker.bytes().map(f64::from).sum::<f64>() % 100.0;
        let step = timeframe_to_ms(timeframe).max(60_000);
        (0..300)
            .map(|i| {
                let open = base + (f64::from(i) * 0.3).sin() * 4.0;
                let close = open + 0.5;
                Ohlcv::new(
                    T0 + i64::from(i) * step,
                    open,
                    open.max(close) + 1.0,
                    open.min(close) - 1.0,
                    close,
                    Some(100.0),
                )
            })
            .collect()
    }
}

impl DataProvider for FakeProvider {
    fn get_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        _range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        self.calls
            .borrow_mut()
            .push((ticker.to_string(), timeframe.to_string()));
        if let Some((tf, default)) = self.refuse.borrow().clone()
            && tf == timeframe
        {
            let mut details = DataUnavailableDetails::new(DataUnavailableReason::Timeframe);
            details.symbol = Some(ticker.to_string());
            details.timeframe = Some(tf);
            details.default = Some(default);
            let err = DataUnavailableError::new(details, None);
            return Box::pin(std::future::ready(Err(DataError::Unavailable(Box::new(
                err,
            )))));
        }
        let bars = Self::bars_for(ticker, timeframe);
        Box::pin(std::future::ready(Ok(bars)))
    }

    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        let list: Vec<SymbolDescriptor> = SYMBOLS
            .iter()
            .map(|t| SymbolDescriptor {
                ticker: (*t).to_string(),
                ..SymbolDescriptor::default()
            })
            .collect();
        Some(Box::pin(std::future::ready(Ok(list))))
    }

    fn get_symbol_info(
        &self,
        _ticker: &str,
    ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
        let info = self.info.borrow().clone();
        info.map(|i| Box::pin(std::future::ready(Ok(Some(i)))) as LocalBoxFuture<'static, _>)
    }
}

pub struct Rig {
    pub platform: ManualPlatform,
    pub host: Rc<TestWorkspaceHost>,
    pub provider: Rc<FakeProvider>,
    pub ws: Workspace,
}

/// Options for a workspace of `layout` whose cells are the named `(identity, symbol)` pairs.
pub fn options(
    provider: &Rc<FakeProvider>,
    layout: &str,
    cells: &[(&str, &str)],
) -> WorkspaceOptions {
    WorkspaceOptions {
        chart: VelaOptions {
            live: Some(false),
            volume: Some(false),
            ..VelaOptions::default()
        },
        layout: LayoutChoice::Id(layout.to_string()),
        cells: cells
            .iter()
            .map(|(id, symbol)| {
                (
                    (*id).to_string(),
                    CellSeed {
                        symbol: Some((*symbol).to_string()),
                        timeframe: Some("60".into()),
                        ..CellSeed::default()
                    },
                )
            })
            .collect(),
        providers: vec![("fake".to_string(), provider.clone() as Rc<dyn DataProvider>)],
        layouts: LayoutRegistry::with_builtins(),
        ..WorkspaceOptions::default()
    }
}

impl Rig {
    pub fn new(layout: &str, cells: &[(&str, &str)]) -> Rig {
        let platform = ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
        let provider = FakeProvider::new();
        let opts = options(&provider, layout, cells);
        Self::with_options(platform, provider, opts)
    }

    pub fn with_options(
        platform: ManualPlatform,
        provider: Rc<FakeProvider>,
        opts: WorkspaceOptions,
    ) -> Rig {
        let env = VelaEnv::new(platform.handle());
        let host = TestWorkspaceHost::new(800.0, 400.0);
        let ws = Workspace::new(host.clone(), env.clone(), opts).expect("a workspace");
        let rig = Rig {
            platform,
            host,
            provider,
            ws,
        };
        rig.settle();
        rig
    }

    /// Run every pending task and every cell's frames until nothing moves.
    pub fn settle(&self) {
        for _ in 0..4 {
            self.platform.run();
            self.host.run_until_idle();
        }
        self.platform.run();
    }

    /// A rig that persists into `storage` under `key`.
    pub fn persisted(
        layout: &str,
        cells: &[(&str, &str)],
        storage: Rc<dyn super::persist::WorkspaceStorage>,
        key: &str,
    ) -> Rig {
        let platform = ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
        let provider = FakeProvider::new();
        let mut opts = options(&provider, layout, cells);
        opts.persist = Some(key.to_string());
        opts.storage = Some(storage);
        Self::with_options(platform, provider, opts)
    }

    pub fn symbols(&self) -> Vec<String> {
        self.ws.cells().iter().map(|c| c.symbol()).collect()
    }

    pub fn ids(&self) -> Vec<String> {
        self.ws.cells().iter().map(|c| c.id().to_string()).collect()
    }
}
