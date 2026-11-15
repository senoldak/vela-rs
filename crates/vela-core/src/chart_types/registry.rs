//! The CHART-TYPE registry: the public extension seam for price styles beyond the
//! built-in set. A chart type may contribute:
//! - a BAR TRANSFORM: a 1:1 time-preserving view over the raw bar stream (Heikin Ashi);
//! - a DATA ENGINE: a per-chart engine that supplies the style's secondary per-bar
//!   data to the renderer's native-layer channels (started/suspended as the style is
//!   entered/left; the seam an order-flow style plugs into);
//! - an extended-ticker MODIFIER: `"SYM;id"` requests the transformed series from
//!   the data plane (`ticker.<id>()` in scripts).
//!
//! The original registry is a process-wide map. Here it is a [`ChartTypeRegistry`]
//! value the caller owns; registration is idempotent-by-id (last registration wins,
//! keeping the original position, as a JavaScript `Map` does).

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::controls::data_control::DataControl;
use crate::i18n::MessageLike;
use crate::indicators::VisibleRange;
use crate::model::Ohlcv;
use crate::ports::market_data::SymbolFacts;
use crate::price_styles::BarTransform;

use super::builtins::register_builtin_chart_types;
use super::settings::ChartTypeSettingsSection;

/// The environment a chart hands to a style's data engine when the style activates.
pub trait SeriesDataEngineHost {
    /// The chart's symbol (as configured, provider prefix stripped by the data layer).
    fn symbol(&self) -> &str;
    /// The chart's timeframe (canonical string, e.g. `"60"`).
    fn timeframe(&self) -> &str;
    /// Whether the chart runs live (streaming forming bar) or static history. False
    /// during a bar replay: entering and leaving replay REBUILDS the engine, so this
    /// never changes within one host's lifetime.
    fn live(&self) -> bool;
    /// The chart's trading session (`"regular"` | `"extended"`); `None` = regular /
    /// no session model. A session switch reloads the market and REBUILDS the
    /// engine, so this never changes within one host's lifetime.
    fn session(&self) -> Option<&str>;
    /// The chart's CURRENT view bars (post-transform): read fresh, never cache.
    fn bars(&self) -> Vec<Ohlcv>;
    /// Push the style's per-bar layer data to the renderer (the `<id>` native channel).
    fn push_data(&self, data: Value);
    /// Push the ranges still loading (the `<id>-pending` channel: skeleton/reveal UIs).
    fn push_pending(&self, ranges: &[(i64, i64)]);
    /// The chart's data surface (providers, capabilities, symbol resolution), as
    /// native indicators get it. A host without one answers with a detached
    /// control that finds nothing.
    fn data(&self) -> DataControl {
        DataControl::detached()
    }
}

/// A per-chart data engine behind a chart type: created on first entry into the
/// style, suspended/resumed as the user switches styles, replaced by a fresh engine
/// when the market or the replay state changes, stopped when the chart is
/// destroyed. All methods are fire-and-forget from the chart's point of view: the
/// engine owns its own scheduling and pushes results through the host channels. The
/// host forwards pushes only from the chart's current, unsuspended engine: work
/// still in flight when the engine is suspended or stopped never reaches the scene.
pub trait SeriesDataEngine {
    fn start(&mut self, host: Arc<dyn SeriesDataEngineHost>);
    /// The style was left, or a market switch began: stop fetching/pushing, keep
    /// state for a cheap resume. Host pushes are dropped from here until `resume()`.
    fn suspend(&mut self);
    /// The style was re-entered: re-push what the layer shows (nothing pushed while
    /// suspended reached it).
    fn resume(&mut self);
    /// The chart is going away, or the engine is being replaced: release
    /// everything. Its host drops every push from here on.
    fn stop(&mut self);
    /// The visible range changed (pan/zoom) while the style is active.
    fn on_viewport(&mut self, _range: VisibleRange) {}
    /// The chart's bars changed while the style is active: a live tick, a new bar (a
    /// replayed one included), or older history prepended. Re-read `host.bars()`; an
    /// engine that streams its own live data may ignore ticks while `host.live()`.
    fn on_bars(&mut self) {}
    /// New settings values from the chart-settings dialog (the type's SDK section).
    /// Also delivered once just BEFORE `start()` whenever stored values exist (a
    /// persisted config, a market switch recreating the engine): an engine must
    /// accept a pre-start call as pure configuration, so it never fetches on schema
    /// defaults the user has edited away.
    fn on_settings(&mut self, _values: &Map<String, Value>) {}
}

/// Factory for a style's per-chart data engine (created lazily on first activation).
pub type DataEngineFactory = Arc<dyn Fn() -> Box<dyn SeriesDataEngine> + Send + Sync>;

/// A chart type's applicability predicate (see [`ChartTypeDefinition::supports`]).
pub type SupportsFn = Arc<dyn Fn(&SymbolFacts) -> bool + Send + Sync>;

/// What the renderer paints for the PRICE SERIES while a style is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BasePainting {
    /// Keeps the base candle painting under the type's layers.
    #[default]
    Candles,
    /// Suppresses it: for types whose renderer layer fully REPLACES the price
    /// representation (an order-flow grid, bricks, ...). Axes, grid, volume,
    /// indicators and drawings are unaffected.
    None,
}

impl BasePainting {
    pub fn as_str(self) -> &'static str {
        match self {
            BasePainting::Candles => "candles",
            BasePainting::None => "none",
        }
    }
}

/// One registered chart type.
#[derive(Clone)]
pub struct ChartTypeDefinition {
    /// The price-style id (`"heikinashi"`; a plugin might register `"footprint"`, `"renko"`...).
    pub id: String,
    /// Human-readable label for style pickers. Defaults to the id. A message
    /// (`msg()`) reads in the chart's language.
    pub label: Option<MessageLike>,
    /// Raw `<svg>` markup for style pickers (surfaced as icon id `style-<id>`).
    pub icon: Option<String>,
    /// Optional SETTINGS SECTION rendered inside the chart-settings dialog:
    /// declarative rows (never DOM). Values persist in the renderer config under
    /// `chartTypes.<id>` and are pushed to the type's `<id>-settings` native-data
    /// channel on change.
    pub settings: Option<ChartTypeSettingsSection>,
    /// View-bar transform applied at the chart's outbound bar seam (1:1,
    /// time-preserving). Callers compare transforms by identity with [`Arc::ptr_eq`].
    pub bar_transform: Option<Arc<dyn BarTransform>>,
    /// Whether the type participates in extended tickers (`"SYM;id"`: scripts
    /// request the transformed series via `ticker.*`). `None` means `true` when
    /// `bar_transform` is set: a transform-based type is exactly what the data plane
    /// can re-derive server-side.
    pub ticker_modifier: Option<bool>,
    /// Factory for the style's per-chart data engine.
    pub data_engine: Option<DataEngineFactory>,
    /// What the renderer paints for the price series while this style is active
    /// (`None` means the default, [`BasePainting::Candles`]).
    pub base_painting: Option<BasePainting>,
    /// Whether the style can draw a symbol, from the facts its data source states
    /// about it (`ohlc: false` for one value per bar, `volume: false` for no
    /// volume). Return `false` and the style menus grey the type out for that
    /// symbol, and a chart set to it shows a close-based style (a line) instead,
    /// without changing the chosen style: the type comes back on the next symbol it
    /// supports. Only called once the facts are known; while they are not, every
    /// type applies. `None` means the type applies to every symbol. Keep it pure and
    /// cheap: the chart calls it on every symbol change and menu build.
    pub supports: Option<SupportsFn>,
}

impl ChartTypeDefinition {
    /// A bare definition: only an id, every capability absent.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            settings: None,
            bar_transform: None,
            ticker_modifier: None,
            data_engine: None,
            base_painting: None,
            supports: None,
        }
    }

    /// Whether the type acts as an extended-ticker modifier.
    pub fn is_ticker_modifier(&self) -> bool {
        self.ticker_modifier.unwrap_or(self.bar_transform.is_some())
    }

    /// What the renderer paints for the price series (defaults to candles).
    pub fn base_painting(&self) -> BasePainting {
        self.base_painting.unwrap_or_default()
    }
}

impl fmt::Debug for ChartTypeDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChartTypeDefinition")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("icon", &self.icon)
            .field("settings", &self.settings)
            .field("bar_transform", &self.bar_transform)
            .field("ticker_modifier", &self.ticker_modifier)
            .field(
                "data_engine",
                &self.data_engine.as_ref().map(|_| "<factory>"),
            )
            .field("base_painting", &self.base_painting)
            .field("supports", &self.supports.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// The set of registered chart types, in registration order.
#[derive(Clone, Debug, Default)]
pub struct ChartTypeRegistry {
    types: Vec<ChartTypeDefinition>,
}

impl ChartTypeRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry holding the built-in chart types (what the original's
    /// `registerBuiltinChartTypes()` registers).
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        register_builtin_chart_types(&mut registry);
        registry
    }

    /// Register (or replace) a chart type. A replaced type keeps its position.
    pub fn register(&mut self, def: ChartTypeDefinition) {
        match self.types.iter_mut().find(|d| d.id == def.id) {
            Some(slot) => *slot = def,
            None => self.types.push(def),
        }
    }

    /// Remove a registered chart type (built-ins can be re-registered via
    /// [`register_builtin_chart_types`]).
    pub fn unregister(&mut self, id: &str) {
        self.types.retain(|d| d.id != id);
    }

    /// The definition behind a price-style id, or `None` for built-in styles and
    /// unknown ids.
    pub fn get(&self, id: &str) -> Option<&ChartTypeDefinition> {
        self.types.iter().find(|d| d.id == id)
    }

    /// Every registered chart type (registration order).
    pub fn all(&self) -> &[ChartTypeDefinition] {
        &self.types
    }

    /// Ids that act as extended-ticker modifiers (`"SYM;id"`).
    pub fn ticker_modifier_ids(&self) -> Vec<String> {
        self.types
            .iter()
            .filter(|d| d.is_ticker_modifier())
            .map(|d| d.id.clone())
            .collect()
    }
}
