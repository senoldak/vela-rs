//! One workspace CELL (`ChartCell.ts`): a stable IDENTITY (its declared name, or `c<N>` for
//! a slot no entry declared) holding a full [`Vela`] chart plus its per-cell state: its own
//! undo timeline, its indicator ledger (shared manifest, per-cell instances) and its
//! persistence. The identity never derives from content: symbol, timeframe and style are
//! mutable state, switched IN PLACE through `chart.set_market` (the chart instance
//! survives every market change and only dies with the cell itself, on a layout change;
//! its state then round-trips through the workspace pool, so shrinking 4 to 2 to 4
//! restores the third and fourth exactly, indicators and drawings included).
//!
//! # Shape
//!
//! [`ChartCell`] is a cheap shared handle. The cell owns its chart; the chart mounts on the
//! [`NativeHost`] the workspace host created for the cell. Everything the cell tells the
//! workspace goes through [`CellObserver`] (the original's `CellDeps` callbacks), held
//! weakly. The cell never holds a `RefCell` borrow across a call into the chart or the
//! observer.
//!
//! # Left for the browser crate
//!
//! The cell's DOM skin is the shell's: `Statusline`, `Watermark`, `CellControls` (the
//! hover cluster), `ChartContextMenu`, the `MarketStatusTracker` and the
//! `SessionShadingTracker` (the last two are widget models that read the chart's data).
//! They bind to the cell through what it exposes: the chart (`cell.try_chart()` events and
//! `chart.data()`), the renderer, and the presentation facts the cell owns:
//! [`ChartCell::status_prefs`] (segment toggles, titles, values, watermarks) with their
//! setters, [`ChartCell::session_overnight`] and [`ChartCell::session_available`],
//! [`ChartCell::exchange_timezone`], [`ChartCell::loading`] and the history
//! ([`ChartCell::history`]). `pushSettingsSections` (the host rows of the settings dialog,
//! built from closures) is not ported: the dialog's rows call the public setters
//! ([`ChartCell::set_session`], [`ChartCell::set_bars`], [`ChartCell::set_session_shade_color`],
//! the status-prefs setters and [`ChartCell::session_shade_color`]). The legend action and
//! callout providers (`registerLegendAction`) belong to the contributions system of the
//! widget port; set them on `cell.try_chart()` and its `renderer()` from the host.
//! `downloadScreenshot` and `screenshotCanvas` are the host's (the native renderer
//! exposes `screenshot_pile`).
//!
//! # Differences from the original
//!
//! - The constructor takes the workspace's resolved pieces in [`CellDeps`] and returns a
//!   `Result` (building a chart can fail).
//! - The manifest entry type is [`ManifestIndicator`] (the widget's `ResolvedIndicator`
//!   lives in the widget port).
//! - Third-party state handlers (`registerStatePersistence`, scope `cell`) are the
//!   [`CellExtHandler`] trait, passed through [`CellDeps::ext_handlers`].
//! - A native indicator's catalog is read asynchronously through the platform, as there
//!   is no promise to `void`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use serde_json::{Map, Value, json};

use crate::data::multi_provider_feed::MultiProviderFeed;
use crate::data_unavailable::DataUnavailableReason;
use crate::drawings::toolbar::{DrawingsConfig, DrawingsOption};
use crate::engine::indicator_handle::IndicatorHandle;
use crate::engine::orchestrator::NativeIndicatorOptions;
use crate::events::Subscription;
use crate::events::types::{ChartEvent, ChartEventName};
use crate::i18n::{LocalizationOptions, MessageArg, MessageLike, Translator, msg, text_of};
use crate::model::{InputSchema, InputValue, Inputs, input_deltas};
use crate::options::{
    AddIndicatorOptions, MarketSwitch, NativeBackend, SettingsVisibilityPolicy, ThemeSpec,
    VelaOptions,
};
use crate::ports::chart_renderer::ChartRenderer;
use crate::ports::market_data::{InitialRange, MarketConfig, MarketSession, SymbolInfo};
use crate::ports::scripting_engine::ScriptingEngine;
use crate::render::native::{NativeHost, NativeParts, NativeRenderer};
use crate::state::{
    CellState, IndicatorsState, PersistedManifestEntry, PersistedNativeEntry, StatuslineParts,
    prefixed_symbol,
};
use crate::timezones::{normalize_timezone, resolve_timezone};
use crate::vela::{IndicatorHandleRef, Vela, VelaDeps, VelaEnv, VelaError};
use crate::visible_range::VisibleRangePreset;

use super::host::ToastKind;
use crate::widget::history::{HistoryAction, WidgetHistory as CellHistory};
use crate::widget::timeframe::{chart_timeframe_of, timeframe_label, timeframe_ms};

/// Scripting-engine factories, instantiated PER CELL (a worker engine per cell).
pub type EngineFactories = Vec<(String, Rc<dyn Fn() -> Rc<dyn ScriptingEngine>>)>;

/// The cell's user-facing messages.
struct Text;

impl Text {
    fn unavailable_fallback() -> MessageLike {
        msg(
            "workspace.data.unavailableFallback",
            "{symbol} is not available at {timeframe}. Showing {shown} instead.",
        )
    }

    fn unavailable() -> MessageLike {
        msg(
            "workspace.data.unavailable",
            "{symbol} is not available at {timeframe}.",
        )
    }

    fn built_in() -> MessageLike {
        msg("workspace.indicators.builtIn", "Built-in")
    }
}

/// Start an in-place market switch and let it run: the switch begins when `set_market` is
/// CALLED and its outcome arrives as the chart's events, so the future is not awaited.
fn start_market(chart: &Vela, next: MarketSwitch) {
    drop(chart.set_market(next));
}

// ── seeds ──────────────────────────────────────────────────────

/// The seed of one cell's market state (all optional: an empty cell parks). The SAME
/// vocabulary as the widget's chart options: the workspace's top-level chart options
/// provide every cell's default ([`seed_defaults`]), `cells` overrides per cell.
/// `data` and `visible_range` are boot-only (they seed the first load, never persist).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellSeed {
    /// Bare ticker (provider resolved by declaration order) or `EXCHANGE:`-prefixed.
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub price_style: Option<String>,
    pub bars: Option<usize>,
    /// Trading session to show (markets that have one; `regular` is the default).
    pub session: Option<String>,
    /// Offline bars for this cell: replaces the provider (boot-only).
    pub data: Option<Vec<crate::model::Ohlcv>>,
    /// Initial visible window (boot-only).
    pub visible_range: Option<InitialRange>,
}

impl CellSeed {
    /// `self` over `base`: every field the entry gives wins (the original's
    /// `{ ...seedDefaults, ...cells[id] }`).
    pub fn over(self, base: CellSeed) -> CellSeed {
        CellSeed {
            symbol: self.symbol.or(base.symbol),
            timeframe: self.timeframe.or(base.timeframe),
            price_style: self.price_style.or(base.price_style),
            bars: self.bars.or(base.bars),
            session: self.session.or(base.session),
            data: self.data.or(base.data),
            visible_range: self.visible_range.or(base.visible_range),
        }
    }
}

/// A destroyed cell's state, kept by the workspace pool so its slot restores later: the
/// per-cell entry of the shared state document.
pub type PooledCellState = CellState;

/// What a cell BOOTS from: a pooled state (restored slot) or an options seed, plus the
/// boot-only extras a pooled state never carries (offline bars, initial window).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellBoot {
    pub state: CellState,
    pub data: Option<Vec<crate::model::Ohlcv>>,
    pub visible_range: Option<InitialRange>,
}

impl From<PooledCellState> for CellBoot {
    fn from(state: PooledCellState) -> Self {
        Self {
            state,
            data: None,
            visible_range: None,
        }
    }
}

impl From<CellSeed> for CellBoot {
    fn from(seed: CellSeed) -> Self {
        Self {
            state: CellState {
                symbol: seed.symbol,
                timeframe: seed.timeframe,
                price_style: seed.price_style,
                bars: seed.bars.map(|b| b as f64),
                session: seed.session,
                ..CellState::default()
            },
            data: seed.data,
            visible_range: seed.visible_range,
        }
    }
}

/// The per-cell SEED the workspace's top-level chart options provide: same words as the
/// widget; `cells[id]` spreads over this.
pub fn seed_defaults(opts: &VelaOptions) -> CellSeed {
    CellSeed {
        symbol: opts.market.symbol.clone(),
        timeframe: opts.market.timeframe.clone(),
        bars: opts.market.bars,
        price_style: opts.price_style.clone(),
        session: opts.market.session.map(|s| s.as_str().to_string()),
        data: opts.market.data.clone(),
        visible_range: opts.market.visible_range,
    }
}

/// Chart options the workspace forwards VERBATIM to every cell's chart: the widget
/// vocabulary minus what the grid manages itself (`height`: the grid sizes cells;
/// `native_backend`: the WebGL budget policy; the market and view seeds, which flow
/// through [`CellSeed`]; and `drawings`' toolbar sub-key, see [`cell_drawings`]).
#[derive(Clone, Default)]
pub struct CellChartDefaults {
    pub renderer: Option<crate::options::RendererConstructor>,
    pub default_language: Option<String>,
    pub current_price_line: Option<bool>,
    pub log_scale: Option<bool>,
    pub animations: Option<crate::options::AnimationsOption>,
    pub glow: Option<f64>,
    pub up_color: Option<String>,
    pub down_color: Option<String>,
    pub drawings: Option<DrawingsOption>,
    pub settings: Option<SettingsVisibilityPolicy>,
    pub price_axis: Option<crate::options::PriceAxisOptions>,
    pub free_pan: Option<crate::options::FreePanTrigger>,
}

/// The [`CellChartDefaults`] pick of a workspace's options (pure, for the build).
pub fn cell_chart_defaults(opts: &VelaOptions) -> CellChartDefaults {
    CellChartDefaults {
        renderer: opts.renderer.clone(),
        default_language: opts.default_language.clone(),
        current_price_line: opts.current_price_line,
        log_scale: opts.log_scale,
        animations: opts.animations,
        glow: opts.glow,
        up_color: opts.up_color.clone(),
        down_color: opts.down_color.clone(),
        drawings: opts.drawings.clone(),
        settings: opts.settings.clone(),
        price_axis: opts.price_axis.clone(),
        free_pan: opts.free_pan.clone(),
    }
}

/// The cell form of the shell's `drawings` option: everything passes through EXCEPT the
/// toolbar: ONE shared bar serves the grid (per-cell bars would cost a 44px gutter each).
/// An explicit `false` stays an opt-out (the headless `chart.drawings` API only).
pub fn cell_drawings(opt: Option<&DrawingsOption>) -> DrawingsOption {
    match opt {
        Some(DrawingsOption::Bool(false)) => DrawingsOption::Bool(false),
        None | Some(DrawingsOption::Bool(true)) => DrawingsOption::Config(DrawingsConfig {
            toolbar: Some(false),
            ..DrawingsConfig::default()
        }),
        Some(DrawingsOption::Config(config)) => DrawingsOption::Config(DrawingsConfig {
            toolbar: Some(false),
            ..config.clone()
        }),
    }
}

// ── small public types ─────────────────────────────────────────

/// A cell's own look prefs as one bundle: the Status line tab's segment toggles (`None`
/// when the shell runs without status lines), the indicator legend titles and values, and
/// the Symbol tab's watermark toggles. What the workspace's STYLE link mirrors across
/// same-group cells, next to the renderer config.
#[derive(Clone, Debug, PartialEq)]
pub struct CellStatusPrefs {
    pub parts: Option<StatuslinePartsOn>,
    pub indicator_titles: bool,
    pub indicator_values: bool,
    pub watermark: bool,
    pub replay_watermark: bool,
}

/// The status line's five segments, resolved (each defaults to shown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatuslinePartsOn {
    pub logo: bool,
    pub name: bool,
    pub market: bool,
    pub ohlc: bool,
    pub change: bool,
}

impl Default for StatuslinePartsOn {
    fn default() -> Self {
        Self {
            logo: true,
            name: true,
            market: true,
            ohlc: true,
            change: true,
        }
    }
}

impl StatuslinePartsOn {
    fn get(&self, part: StatuslinePart) -> bool {
        match part {
            StatuslinePart::Logo => self.logo,
            StatuslinePart::Name => self.name,
            StatuslinePart::Market => self.market,
            StatuslinePart::Ohlc => self.ohlc,
            StatuslinePart::Change => self.change,
        }
    }

    fn set(&mut self, part: StatuslinePart, on: bool) {
        match part {
            StatuslinePart::Logo => self.logo = on,
            StatuslinePart::Name => self.name = on,
            StatuslinePart::Market => self.market = on,
            StatuslinePart::Ohlc => self.ohlc = on,
            StatuslinePart::Change => self.change = on,
        }
    }

    fn persisted(&self) -> StatuslineParts {
        StatuslineParts {
            logo: Some(self.logo),
            name: Some(self.name),
            market: Some(self.market),
            ohlc: Some(self.ohlc),
            change: Some(self.change),
        }
    }
}

/// One segment of the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatuslinePart {
    Logo,
    Name,
    Market,
    Ohlc,
    Change,
}

impl StatuslinePart {
    pub const ALL: [StatuslinePart; 5] = [
        Self::Logo,
        Self::Name,
        Self::Market,
        Self::Ohlc,
        Self::Change,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::Name => "name",
            Self::Market => "market",
            Self::Ohlc => "ohlc",
            Self::Change => "change",
        }
    }
}

/// One entry of the shared indicator picker's native catalog, per cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellNativeInfo {
    pub indicator_type: String,
    pub title: String,
    pub supported: bool,
    pub present: bool,
    pub beta: bool,
}

/// A script indicator of the shared manifest: the widget port's `ResolvedIndicator` (an
/// `indicators` option entry after its source was fetched).
pub type ManifestIndicator = crate::widget::indicators::ResolvedIndicator;

/// A script indicator added through the public seam (`ctx.addIndicator`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExternalIndicatorEntry {
    pub name: String,
    pub script: String,
    pub language: Option<String>,
    pub category: Option<String>,
    /// The id the indicator runs under (host-supplied).
    pub id: Option<String>,
    pub inputs: Option<Inputs>,
    pub props: Option<Inputs>,
    pub hidden: bool,
}

/// A bottombar range chip: switch to its timeframe, fetch the depth its window needs.
#[derive(Clone, Debug, PartialEq)]
pub struct CellRangePreset {
    pub id: String,
    pub tf: String,
    pub bars: usize,
    pub preset: VisibleRangePreset,
}

/// A row of the indicator picker's library list (natives first, then the manifest).
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryRow {
    pub name: String,
    pub aliases: Vec<String>,
    pub language: Option<String>,
    pub category: Option<String>,
    pub native: bool,
    pub native_type: Option<String>,
    pub beta: bool,
}

/// A row of the indicator picker's on-chart list (native instances, then scripts).
#[derive(Clone, Debug, PartialEq)]
pub struct OnChartRow {
    pub name: String,
    pub aliases: Vec<String>,
    pub language: Option<String>,
    pub native: bool,
    pub native_type: Option<String>,
}

/// Third-party per-chart state (`registerStatePersistence` with scope `cell`). Values are
/// OPAQUE to the codec; each handler validates its own payload at restore.
pub trait CellExtHandler {
    /// The namespaced key (`vendor.feature`).
    fn key(&self) -> &str;
    /// The value to persist, or `None` to withdraw the entry.
    fn serialize(&self, cell: &ChartCell) -> Option<Value>;
    /// Re-apply a persisted value. Runs muted: nothing it does enters the undo timeline.
    fn restore(&self, value: &Value, cell: &ChartCell);
}

/// What the cell tells the workspace (the original's `CellDeps` callbacks). Methods that
/// answer a question have the workspace's current value.
pub trait CellObserver {
    /// A pointer-down or focus in this cell: the workspace makes it active.
    fn activate(&self, id: &str);
    /// The workspace-global display-timezone CHOICE (an IANA zone, or the exchange rule).
    fn timezone(&self) -> String;
    /// Switch the workspace-global display timezone.
    fn set_timezone(&self, zone: &str);
    /// The shared manifest can no longer change instance sets.
    fn manifest_settled(&self) -> bool;
    fn market_changed(&self, id: &str);
    fn price_style_changed(&self, id: &str);
    fn price_style_shown(&self, id: &str);
    fn indicators_changed(&self, id: &str);
    /// The native-indicator catalog was re-read (its supported and present flags): the
    /// picker re-reads it. Nothing persistable changed, so the document is not dirtied (the
    /// original marks it dirty here, which makes a booted workspace announce an edit nobody
    /// made).
    fn catalog_changed(&self, id: &str);
    fn status_prefs_changed(&self, id: &str);
    /// Persistable per-cell state changed outside the market and indicator channels.
    fn state_dirty(&self);
    fn toast(&self, message: &str, kind: ToastKind, duration_ms: f64);
}

/// What every cell shares from the workspace.
pub struct CellDeps {
    pub observer: Weak<dyn CellObserver>,
    pub env: VelaEnv,
    /// The host the cell's chart mounts on (made by `WorkspaceHost::create_cell`).
    pub native_host: Rc<dyn NativeHost>,
    /// THE shared market-data registry feed (one registry, one cache, for every cell).
    pub feed: MultiProviderFeed,
    pub engines: EngineFactories,
    /// The workspace's top-level chart options every cell's chart starts from.
    pub chart_defaults: CellChartDefaults,
    /// The workspace's CURRENT localization: a cell built after a `set_locale` starts in
    /// it.
    pub localization: LocalizationOptions,
    pub theme: ThemeSpec,
    pub live: bool,
    pub volume: bool,
    pub statusline: bool,
    pub watermark: bool,
    /// Geometry backend for cells under the current layout (the WebGL budget policy).
    pub native_backend: NativeBackend,
    pub ext_handlers: Vec<Rc<dyn CellExtHandler>>,
}

// ── ledger helpers ─────────────────────────────────────────────

/// The entry's manifest NAME, whichever shape it travels as.
pub fn ledger_entry_name(e: &PersistedManifestEntry) -> &str {
    match e {
        PersistedManifestEntry::Name(n) => n,
        PersistedManifestEntry::Entry { name, .. } => name,
    }
}

/// The entry's native TYPE, whichever shape it travels as.
pub fn ledger_native_type(e: &PersistedNativeEntry) -> &str {
    match e {
        PersistedNativeEntry::Type(t) => t,
        PersistedNativeEntry::Entry { kind, .. } => kind,
    }
}

/// Everything [`indicator_ledger`] needs to decide what a state snapshot reports.
pub struct LedgerInputs {
    /// Native entries present on the chart RIGHT NOW (live handles: type, input deltas,
    /// hidden).
    pub present: Vec<PersistedNativeEntry>,
    /// The live manifest instances, values included.
    pub instance_entries: Vec<PersistedManifestEntry>,
    /// A restored ledger's manifest half, until it materializes (`None` once consumed).
    pub pending_manifest: Option<Vec<PersistedManifestEntry>>,
    /// The manifest can no longer change the instance set. Until then the pending names
    /// are reported as they are: an early save must not wipe a restored ledger the shell
    /// simply has not materialized yet.
    pub manifest_settled: bool,
    /// The volume auto-add is still owed (before the first `load:end` while the intent
    /// says volume, or after it while the chart waits for the symbol's facts). The
    /// registry cannot show it yet; without this, an early save would write no natives and
    /// the reload would read it as a deliberate opt-out, silencing volume forever.
    pub volume_pending: bool,
}

/// The indicator ledger a state snapshot persists. The one rule: a LIVE (possibly empty)
/// set is the truth once its source settled; fallbacks cover ONLY the boot window before a
/// restored ledger materializes. "Empty because the user removed everything" must never be
/// repainted as "empty because nothing loaded yet".
pub fn indicator_ledger(i: LedgerInputs) -> IndicatorsState {
    let mut natives = i.present;
    if i.volume_pending && !natives.iter().any(|e| ledger_native_type(e) == "volume") {
        natives.push(PersistedNativeEntry::Type("volume".into()));
    }
    let manifest = if i.manifest_settled {
        i.instance_entries
    } else {
        i.pending_manifest.unwrap_or(i.instance_entries)
    };
    IndicatorsState { manifest, natives }
}

fn inputs_to_map(inputs: &Inputs) -> Map<String, Value> {
    inputs
        .iter()
        .map(|(k, v)| {
            let value = match v {
                InputValue::Number(n) => {
                    serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number)
                }
                InputValue::Text(s) => Value::String(s.clone()),
                InputValue::Bool(b) => Value::Bool(*b),
            };
            (k.clone(), value)
        })
        .collect()
}

/// Untrusted persisted values: only numbers, strings and booleans are input values.
fn map_to_inputs(map: &Map<String, Value>) -> Inputs {
    map.iter()
        .filter_map(|(k, v)| {
            let value = match v {
                Value::Number(n) => InputValue::Number(n.as_f64()?),
                Value::String(s) => InputValue::Text(s.clone()),
                Value::Bool(b) => InputValue::Bool(*b),
                _ => return None,
            };
            Some((k.clone(), value))
        })
        .collect()
}

/// A native handle's ledger entry: the bare type, or type + input deltas + hidden flag.
fn native_ledger_entry(handle: &dyn IndicatorHandle) -> Option<PersistedNativeEntry> {
    let kind = handle.native_type()?.to_string();
    let inputs = input_deltas(&handle.inputs(), &handle.input_values());
    let hidden = !handle.visible();
    Some(if inputs.is_some() || hidden {
        PersistedNativeEntry::Entry {
            kind,
            inputs: inputs.as_ref().map(inputs_to_map),
            hidden: hidden.then_some(true),
        }
    } else {
        PersistedNativeEntry::Type(kind)
    })
}

/// The values an instance deviates with from its declaration defaults.
#[derive(Clone, Debug, Default, PartialEq)]
struct InstanceValues {
    inputs: Option<Inputs>,
    props: Option<Inputs>,
}

/// The handle's current input/prop DELTAS against declaration defaults.
fn instance_deltas(handle: &dyn IndicatorHandle) -> Option<InstanceValues> {
    let inputs = input_deltas(&handle.inputs(), &handle.input_values());
    let props = input_deltas(&handle.props(), &handle.prop_values());
    (inputs.is_some() || props.is_some()).then_some(InstanceValues { inputs, props })
}

/// The verdict of `parseSessionSpec(si)?.overnight` (the session-shading model of the
/// widget port): an extended tape written `HHMM-HHMM` that wraps midnight, evening opening
/// after the regular close and the same-day tail reaching at least that close.
fn session_overnight(si: &SymbolInfo) -> bool {
    let window = |text: &str, allow_overnight: bool| -> Option<(u32, u32)> {
        let b = text.as_bytes();
        if b.len() != 9 || b[4] != b'-' {
            return None;
        }
        let digits = |s: &[u8]| -> Option<u32> {
            s.iter().try_fold(0u32, |acc, c| {
                c.is_ascii_digit().then(|| acc * 10 + u32::from(c - b'0'))
            })
        };
        let (h1, m1, h2, m2) = (
            digits(&b[0..2])?,
            digits(&b[2..4])?,
            digits(&b[5..7])?,
            digits(&b[7..9])?,
        );
        let (start, end) = (h1 * 60 + m1, h2 * 60 + m2);
        if end > 1440 || start >= 1440 || (start >= end && !allow_overnight) {
            return None;
        }
        Some((start, end))
    };
    let Some(session) = si.session.as_deref() else {
        return false;
    };
    if session.is_empty() || session == "24x7" {
        return false;
    }
    let Some(regular) = window(session, false) else {
        return false;
    };
    let Some(ext) = si.session_extended.as_deref().and_then(|e| window(e, true)) else {
        return false;
    };
    ext.0 >= ext.1 && ext.0 >= regular.1 && ext.1 >= regular.1
}

// ── the cell ───────────────────────────────────────────────────

/// One live manifest/external instance and, when it deviates from declaration defaults,
/// the values it was restored with or dropped holding.
struct CellInstance {
    entry: ManifestIndicator,
    handle: RefCell<Option<IndicatorHandleRef>>,
    /// The id the indicator runs under on the chart: recorded on the first add (host
    /// supplied or minted) so an undo/redo resurrection re-adds it under the SAME id.
    id: RefCell<Option<String>>,
    external: bool,
    values: RefCell<Option<InstanceValues>>,
}

/// The cell's durable market state: the seed vocabulary plus the venue mirror the
/// persisted document carries.
#[derive(Clone, Debug, Default)]
struct MarketState {
    symbol: Option<String>,
    provider: Option<String>,
    timeframe: Option<String>,
    price_style: Option<String>,
    bars: Option<f64>,
    session: Option<MarketSession>,
}

struct CellData {
    state: MarketState,
    /// Latched verdict of `session_available` (async metadata, sticky per symbol).
    session_available: bool,
    /// The symbol of the most recent metadata probe: an older one landing late is dropped.
    metadata_probe_symbol: Option<String>,
    /// The symbol's extended tape wraps midnight (an overnight roll market).
    session_overnight: bool,
    /// The symbol's own trading zone (`SymbolInfo.timezone`).
    exchange_zone: Option<String>,
    manifest: Vec<ManifestIndicator>,
    /// A restored ledger's manifest entries, waiting for the manifest to resolve.
    pending_manifest: Option<Vec<PersistedManifestEntry>>,
    /// The volume auto-add rides the cell's first candles (`load:end`); until then the
    /// registry cannot show it and the dehydrated ledger reports the INTENT instead.
    volume_may_be_pending: bool,
    volume_intent: bool,
    /// Sync mirror of the chart's native instances (id + type): the removal handler looks
    /// the removed id up here to learn which type an undo must re-add.
    present_natives: Vec<(String, String)>,
    range_bars: usize,
    pending_range: Option<CellRangePreset>,
    /// Last symbol we toasted "no provider serves this" for: once per symbol.
    unresolved_toasted: Option<String>,
    /// The timeframe the chart RUNS for a symbol whose source refused the requested one
    /// (`data:unavailable`). The requested timeframe stays in `state.timeframe` (what the
    /// cell persists and the timeframe link shares).
    fallback: Option<(String, String)>,
    ext_state: BTreeMap<String, Value>,
    watermark_on: bool,
    replay_watermark_on: bool,
    indicator_titles_on: bool,
    indicator_values_on: bool,
    /// `None` when the shell runs without status lines.
    statusline_parts: Option<StatuslinePartsOn>,
    instances: Vec<Rc<CellInstance>>,
    native_catalog: Vec<CellNativeInfo>,
    loading: bool,
    /// Last crosshair position in this cell (the alt+H and alt+V shortcuts anchor here).
    last_cross_time: Option<f64>,
    last_cross_price: Option<f64>,
    /// The bottombar range chip this cell is framed on.
    active_range_id: Option<String>,
}

struct CellInner {
    id: String,
    deps: CellDeps,
    chart: RefCell<Option<Vela>>,
    renderer: RefCell<Option<Rc<NativeRenderer>>>,
    history: CellHistory,
    data: RefCell<CellData>,
    subs: RefCell<Vec<Subscription<ChartEventName>>>,
    offs: RefCell<Vec<crate::runtime::Unsubscribe>>,
    destroyed: Cell<bool>,
}

/// A workspace cell. A cheap shared handle; see the module documentation.
#[derive(Clone)]
pub struct ChartCell {
    inner: Rc<CellInner>,
}

/// A non-owning handle to a [`ChartCell`].
#[derive(Clone)]
pub struct WeakChartCell {
    inner: Weak<CellInner>,
}

impl WeakChartCell {
    pub fn upgrade(&self) -> Option<ChartCell> {
        self.inner.upgrade().map(|inner| ChartCell { inner })
    }
}

fn session_of(raw: Option<&str>) -> Option<MarketSession> {
    raw.and_then(MarketSession::normalize)
}

impl ChartCell {
    /// Build cell `id` from `boot` on the workspace's resolved pieces.
    pub fn new(id: &str, boot: CellBoot, deps: CellDeps) -> Result<ChartCell, VelaError> {
        let seed = boot.state.clone();
        // The canonical symbol form: pre-prefix pooled/persisted states carried the venue
        // in `provider` beside a bare symbol; weld them back together once, at boot.
        let symbol = prefixed_symbol(Some(&seed));
        let session = session_of(seed.session.as_deref());
        let slot: Rc<RefCell<Option<Rc<NativeRenderer>>>> = Rc::new(RefCell::new(None));
        let renderer_ctor: crate::options::RendererConstructor = {
            let slot = slot.clone();
            let native = deps.native_host.clone();
            let parts = NativeParts {
                chart_types: deps.env.registries.chart_types.clone(),
                ..NativeParts::default()
            };
            Rc::new(move |display| {
                let renderer =
                    NativeRenderer::with_parts(native.clone(), Some(display), parts.clone());
                *slot.borrow_mut() = Some(renderer.clone());
                renderer as Rc<dyn ChartRenderer>
            })
        };
        let defaults = &deps.chart_defaults;
        // The renderer's own Time zone row edits a resolved IANA zone; the cell
        // contributes the workspace picker instead (`time-zone`, with the exchange rule).
        let mut hidden = defaults
            .settings
            .as_ref()
            .and_then(|s| s.hidden.clone())
            .unwrap_or_default();
        hidden.push("symbol.timezone".into());
        let volume = match &seed.indicators {
            Some(ledger) => ledger
                .natives
                .iter()
                .any(|e| ledger_native_type(e) == "volume"),
            None => deps.volume,
        };
        let options = VelaOptions {
            market: MarketConfig {
                symbol: symbol.clone(),
                timeframe: seed.timeframe.clone(),
                bars: seed
                    .bars
                    .filter(|b| b.is_finite() && *b > 0.0)
                    .map(|b| b as usize),
                session,
                visible_range: boot.visible_range,
                data: boot.data.clone(),
            },
            localization: deps.localization.clone(),
            live: Some(deps.live),
            theme: Some(deps.theme.clone()),
            height: None,
            renderer: Some(renderer_ctor),
            default_language: defaults.default_language.clone(),
            current_price_line: defaults.current_price_line,
            log_scale: defaults.log_scale,
            native_backend: Some(deps.native_backend),
            animations: defaults.animations,
            glow: defaults.glow,
            up_color: defaults.up_color.clone(),
            down_color: defaults.down_color.clone(),
            price_style: seed.price_style.clone(),
            // The user's drawings option minus its toolbar: one SHARED bar serves the
            // whole workspace.
            drawings: Some(cell_drawings(defaults.drawings.as_ref())),
            volume: Some(volume),
            settings: Some(SettingsVisibilityPolicy {
                hidden: Some(hidden),
            }),
            price_axis: defaults.price_axis.clone(),
            free_pan: defaults.free_pan.clone(),
        };
        let chart = Vela::with_deps(
            &deps.env,
            options,
            VelaDeps {
                registry_feed: Some(deps.feed.clone()),
                ..VelaDeps::default()
            },
        )?;
        let renderer = slot.borrow().clone();

        let provider = symbol
            .as_deref()
            .and_then(|s| crate::data::provider_registry::parse_symbol(s).provider);
        let parts = deps.statusline.then(|| {
            let mut on = StatuslinePartsOn::default();
            let saved = seed.statusline_parts.unwrap_or_default();
            for (part, v) in [
                (StatuslinePart::Logo, saved.logo),
                (StatuslinePart::Name, saved.name),
                (StatuslinePart::Market, saved.market),
                (StatuslinePart::Ohlc, saved.ohlc),
                (StatuslinePart::Change, saved.change),
            ] {
                if let Some(v) = v {
                    on.set(part, v);
                }
            }
            on
        });
        let data = CellData {
            state: MarketState {
                symbol,
                provider,
                timeframe: seed.timeframe.clone(),
                price_style: seed.price_style.clone(),
                bars: seed.bars,
                session,
            },
            session_available: false,
            metadata_probe_symbol: None,
            session_overnight: false,
            exchange_zone: None,
            manifest: Vec::new(),
            pending_manifest: seed.indicators.as_ref().map(|l| l.manifest.clone()),
            volume_may_be_pending: true,
            volume_intent: volume,
            present_natives: Vec::new(),
            range_bars: 0,
            pending_range: None,
            unresolved_toasted: None,
            fallback: None,
            ext_state: seed.ext.clone().unwrap_or_default(),
            watermark_on: seed.watermark.unwrap_or(deps.watermark),
            replay_watermark_on: seed.replay_watermark.unwrap_or(true),
            indicator_titles_on: seed.indicator_titles.unwrap_or(true),
            indicator_values_on: seed.indicator_values.unwrap_or(true),
            statusline_parts: parts,
            instances: Vec::new(),
            native_catalog: Vec::new(),
            loading: false,
            last_cross_time: None,
            last_cross_price: None,
            active_range_id: None,
        };
        let weak_for_history: Weak<CellInner> = Weak::new();
        let _ = weak_for_history;
        let chart_for_history = chart.downgrade();
        let history = CellHistory::new(move || chart_for_history.upgrade());
        let inner = Rc::new(CellInner {
            id: id.to_string(),
            deps,
            chart: RefCell::new(Some(chart.clone())),
            renderer: RefCell::new(renderer),
            history,
            data: RefCell::new(data),
            subs: RefCell::new(Vec::new()),
            offs: RefCell::new(Vec::new()),
            destroyed: Cell::new(false),
        });
        let cell = ChartCell { inner };
        cell.wire(&chart, &seed);
        Ok(cell)
    }

    // ── construction wiring ──

    fn wire(&self, chart: &Vela, seed: &CellState) {
        let inner = &self.inner;
        for (language, make) in &inner.deps.engines {
            chart.register_engine(language, make());
        }
        // ONE attribution mark per WORKSPACE, not per cell: each cell disables its own
        // in-chart mark; the workspace host mounts the single grid-level mark that
        // satisfies the NOTICE's equivalent-visible-attribution requirement.
        chart.renderer().set("attribution", json!(false));
        // The cell owns ONE unified undo timeline (drawings + indicator ops), driven by
        // the workspace keymap. The drawings layer must not self-serve its own undo chords
        // or the two histories desync.
        if chart.renderer().supports("historyChords") {
            chart.renderer().set("historyChords", json!(false));
        }
        inner.history.on_chart(chart);
        // A config document can carry a time zone and a price style too (a template
        // import, a headless applyConfig): mirror them back so the workspace bottom bar,
        // the other cells and the persisted state never disagree with this cell's axis.
        let weak = self.downgrade();
        let off = chart.renderer().on_config_changed(Box::new(move || {
            if let Some(cell) = weak.upgrade() {
                cell.on_config_changed();
            }
        }));
        inner.offs.borrow_mut().push(off);

        // Pool restore: cosmetics + drawings round-trip (both validate untrusted input).
        if let Some(config) = &seed.renderer_config {
            chart.renderer().apply_config(config);
        }
        if let Some(doc) = &seed.drawings {
            chart.drawings().from_json(doc);
        }
        // A restored ledger: natives re-add immediately (registry truth from here on);
        // manifest entries wait for set_manifest and stay reported by `dehydrate` until
        // then, so an early snapshot never wipes them.
        if let Some(ledger) = &seed.indicators {
            for entry in &ledger.natives {
                let handle = chart.add_native_indicator(
                    ledger_native_type(entry),
                    NativeIndicatorOptions::default(),
                );
                Self::apply_native_ledger_entry(handle.as_ref(), entry);
            }
        }
        self.apply_timezone();

        self.subscribe(chart, ChartEventName::ThemeChanged, |_, _| {});
        self.subscribe(chart, ChartEventName::LoadEnd, |cell, _| {
            cell.inner.data.borrow_mut().volume_may_be_pending = false;
            cell.inner.data.borrow_mut().loading = false;
        });
        self.subscribe(chart, ChartEventName::DataUnresolved, |cell, ev| {
            let ChartEvent::DataUnresolved { symbol, providers } = ev else {
                return;
            };
            // The feed is shared, so EVERY cell's chart hears another cell's unresolved
            // symbol (the original toasts it N times); only the cell that asked for it speaks.
            let own = cell.try_chart().and_then(|c| c.market().symbol);
            if own.as_deref() != Some(symbol.as_str()) {
                return;
            }
            {
                let mut d = cell.inner.data.borrow_mut();
                if d.unresolved_toasted.as_deref() == Some(symbol) {
                    return;
                }
                d.unresolved_toasted = Some(symbol.clone());
            }
            let list = if providers.is_empty() {
                "none".to_string()
            } else {
                providers.join(", ")
            };
            cell.toast(
                &format!("No registered provider serves \"{symbol}\" (registered: {list})"),
                ToastKind::Error,
                6000.0,
            );
        });
        self.subscribe(chart, ChartEventName::DataUnavailable, |cell, ev| {
            cell.on_data_unavailable(ev);
        });
        self.subscribe(chart, ChartEventName::SymbolFacts, |cell, _| {
            if let Some(obs) = cell.inner.deps.observer.upgrade() {
                obs.price_style_shown(&cell.inner.id);
            }
        });
        self.subscribe(chart, ChartEventName::LoadStart, |cell, ev| {
            let ChartEvent::LoadStart { symbol, .. } = ev else {
                return;
            };
            cell.inner.data.borrow_mut().loading = true;
            // Probe the new symbol's metadata HERE rather than waiting for
            // `market:changed` (which fires only once the new bars are painted): the
            // session toggle and the exchange zone then settle while the bars load.
            cell.refresh_symbol_metadata(Some(symbol.clone()), true);
        });
        // The venue chip is provisional (persisted or typed prefix): once the shared
        // feed's indexes settle, re-derive from the DATA.
        {
            let weak = self.downgrade();
            let ready = chart.data().ready();
            inner.deps.env.platform.spawn(Box::pin(async move {
                ready.await;
                if let Some(cell) = weak.upgrade() {
                    cell.refresh_symbol_metadata(None, true);
                }
            }));
        }
        {
            let weak = self.downgrade();
            let off = chart.renderer().on_crosshair_move(Box::new(
                move |e: &crate::ports::chart_renderer::CrosshairEvent| {
                    if let Some(cell) = weak.upgrade() {
                        let mut d = cell.inner.data.borrow_mut();
                        d.last_cross_time = e.time.map(|t| t as f64);
                        d.last_cross_price = e.price;
                    }
                },
            ));
            inner.offs.borrow_mut().push(off);
        }
        self.subscribe(chart, ChartEventName::IndicatorAdded, |cell, _| {
            cell.sync_present_natives();
            cell.refresh_native_catalog();
        });
        self.subscribe(chart, ChartEventName::IndicatorInputs, |cell, _| {
            cell.state_dirty();
        });
        // A legend-eye toggle is a persistable choice like an input edit.
        self.subscribe(chart, ChartEventName::IndicatorVisibility, |cell, _| {
            cell.state_dirty();
        });
        self.subscribe(chart, ChartEventName::IndicatorRemoved, |cell, ev| {
            if let ChartEvent::IndicatorRemoved { id } = ev {
                cell.on_indicator_removed(id);
            }
        });
        self.sync_present_natives();
        self.refresh_native_catalog();
        // The committed bookkeeping seam: every market change (cell setters, sync links,
        // or host code calling chart.set_market directly) lands here and updates the
        // cell state, then notifies the workspace.
        self.subscribe(chart, ChartEventName::MarketChanged, |cell, ev| {
            let ChartEvent::MarketChanged {
                symbol, timeframe, ..
            } = ev
            else {
                return;
            };
            cell.project_market(symbol, timeframe);
            cell.refresh_native_catalog();
            cell.refresh_symbol_metadata(None, true);
            if let Some(obs) = cell.inner.deps.observer.upgrade() {
                obs.market_changed(&cell.inner.id);
            }
        });
    }

    fn subscribe(
        &self,
        chart: &Vela,
        name: ChartEventName,
        f: impl Fn(&ChartCell, &ChartEvent) + 'static,
    ) {
        let weak = self.downgrade();
        let sub = chart.on(name, move |ev| {
            if let Some(cell) = weak.upgrade()
                && !cell.inner.destroyed.get()
            {
                f(&cell, ev);
            }
        });
        self.inner.subs.borrow_mut().push(sub);
    }

    pub fn downgrade(&self) -> WeakChartCell {
        WeakChartCell {
            inner: Rc::downgrade(&self.inner),
        }
    }

    fn observer(&self) -> Option<Rc<dyn CellObserver>> {
        self.inner.deps.observer.upgrade()
    }

    fn toast(&self, message: &str, kind: ToastKind, ms: f64) {
        if let Some(obs) = self.observer() {
            obs.toast(message, kind, ms);
        }
    }

    fn state_dirty(&self) {
        if let Some(obs) = self.observer() {
            obs.state_dirty();
        }
    }

    fn on_config_changed(&self) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let zone = chart
            .renderer()
            .get("timezone")
            .and_then(|v| v.as_str().map(str::to_string));
        if let Some(zone) = zone
            && normalize_timezone(&zone) != normalize_timezone(&self.display_timezone())
            && let Some(obs) = self.observer()
        {
            obs.set_timezone(normalize_timezone(&zone));
        }
        // A document can carry the price style too (a template import): the renderer is
        // already showing it, so adopt it and re-project the chrome.
        let style = self.requested_price_style();
        let changed = {
            let mut d = self.inner.data.borrow_mut();
            let held = d
                .state
                .price_style
                .clone()
                .unwrap_or_else(|| "candles".into());
            if style != held {
                d.state.price_style = Some(style);
                true
            } else {
                false
            }
        };
        if changed && let Some(obs) = self.observer() {
            obs.price_style_changed(&self.inner.id);
        }
    }

    // ── access ──

    /// The cell's identity.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The LIVE chart of this cell (`None` once destroyed). Never cache it across a layout
    /// change: the cell's identity is what endures; the chart dies with the cell.
    pub fn try_chart(&self) -> Option<Vela> {
        self.inner.chart.borrow().clone()
    }

    /// The native renderer mounted for this cell's chart (the same one the host's
    /// `cell_mounted` received).
    pub fn renderer(&self) -> Option<Rc<NativeRenderer>> {
        self.inner.renderer.borrow().clone()
    }

    pub fn is_destroyed(&self) -> bool {
        self.inner.destroyed.get()
    }

    /// Whether `other` is a handle to this very cell (not merely one of the same identity:
    /// a rebuilt cell is a new one).
    pub fn is_same(&self, other: &ChartCell) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// This cell's unified undo timeline (the shared undo chord routes here).
    pub fn history(&self) -> &CellHistory {
        &self.inner.history
    }

    pub fn symbol(&self) -> String {
        self.inner
            .data
            .borrow()
            .state
            .symbol
            .clone()
            .unwrap_or_default()
    }

    /// The timeframe the chart shows: the requested one, or the fallback while the
    /// symbol's source refuses it.
    pub fn timeframe(&self) -> String {
        let d = self.inner.data.borrow();
        d.fallback
            .as_ref()
            .map(|(_, tf)| tf.clone())
            .or_else(|| d.state.timeframe.clone())
            .unwrap_or_else(|| "60".into())
    }

    /// The timeframe the user asked for: what the cell persists and the timeframe link
    /// shares. Differs from [`timeframe`](Self::timeframe) while the source refuses it.
    pub fn requested_timeframe(&self) -> String {
        self.inner
            .data
            .borrow()
            .state
            .timeframe
            .clone()
            .unwrap_or_else(|| "60".into())
    }

    /// The venue prefix of the symbol (`binance` for `binance:BTCUSDT`).
    pub fn provider(&self) -> Option<String> {
        self.inner.data.borrow().state.provider.clone()
    }

    /// The price style the chart draws: the requested one, or its close-based fallback
    /// while the symbol's data cannot draw it.
    pub fn price_style(&self) -> String {
        let shown = self
            .try_chart()
            .and_then(|c| c.renderer().get("shownPriceStyle"))
            .and_then(|v| v.as_str().map(str::to_string));
        shown.unwrap_or_else(|| self.requested_price_style())
    }

    /// The price style the user chose: what the cell persists.
    pub fn requested_price_style(&self) -> String {
        let live = self
            .try_chart()
            .and_then(|c| c.renderer().get("priceStyle"))
            .and_then(|v| v.as_str().map(str::to_string));
        live.unwrap_or_else(|| {
            self.inner
                .data
                .borrow()
                .state
                .price_style
                .clone()
                .unwrap_or_else(|| "candles".into())
        })
    }

    /// The style ids the symbol's data cannot draw: greyed in the style pickers. Empty
    /// while the symbol's facts are unknown.
    pub fn unavailable_price_styles(&self) -> Vec<String> {
        let Some(chart) = self.try_chart() else {
            return Vec::new();
        };
        let Some(facts) = chart.symbol_facts() else {
            return Vec::new();
        };
        let registry = chart.env().registries.chart_types.clone();
        let registry = registry.borrow();
        crate::render::core::chart_config::price_style_ids(&registry)
            .into_iter()
            .filter(|id| !crate::chart_types::price_style_applies(&registry, id, Some(&facts)))
            .collect()
    }

    /// Manifest instances + native instances: the topbar indicator count.
    pub fn indicator_count(&self) -> usize {
        let natives = if self.try_chart().is_some() {
            self.native_handles().len()
        } else {
            self.inner.data.borrow().present_natives.len()
        };
        self.inner.data.borrow().instances.len() + natives
    }

    /// The bottombar range chip this cell is framed on.
    pub fn active_range_id(&self) -> Option<String> {
        self.inner.data.borrow().active_range_id.clone()
    }

    /// Last crosshair position in this cell.
    pub fn last_crosshair(&self) -> (Option<f64>, Option<f64>) {
        let d = self.inner.data.borrow();
        (d.last_cross_time, d.last_cross_price)
    }

    /// A load is in flight with nothing painted (the watermark and the loading
    /// affordance never share the canvas).
    pub fn loading(&self) -> bool {
        self.inner.data.borrow().loading
    }

    /// Whether this cell's market HAS sessions (RTH/ETH meaningful), from the symbol's own
    /// metadata, asynchronously.
    pub fn session_available(&self) -> bool {
        self.inner.data.borrow().session_available
    }

    /// The symbol's extended tape wraps midnight: one extended-hours shading phase
    /// instead of the pre/post split.
    pub fn session_overnight(&self) -> bool {
        self.inner.data.borrow().session_overnight
    }

    /// This cell's shown session (`Regular` when unset: the provider default).
    pub fn session(&self) -> MarketSession {
        self.inner
            .data
            .borrow()
            .state
            .session
            .unwrap_or(MarketSession::Regular)
    }

    /// The symbol's own trading zone, once its metadata has landed: what the exchange rule
    /// resolves to on this cell. `None` = unknown or undeclared (the rule then renders
    /// UTC).
    pub fn exchange_timezone(&self) -> Option<String> {
        self.inner.data.borrow().exchange_zone.clone()
    }

    /// The IANA zone this cell's axis renders in: the workspace choice, resolved.
    pub fn display_timezone(&self) -> String {
        let choice = self
            .observer()
            .map_or_else(|| "Etc/UTC".to_string(), |o| o.timezone());
        resolve_timezone(&choice, self.exchange_timezone().as_deref())
    }

    /// The cell's bars-to-fetch budget (`None` = the chart default).
    pub fn bars(&self) -> Option<f64> {
        self.inner.data.borrow().state.bars
    }

    /// The registered native-indicator catalog with this cell's live supported/present
    /// flags (refreshed asynchronously; see [`refresh_native_catalog`](Self::refresh_native_catalog)).
    pub fn native_catalog(&self) -> Vec<CellNativeInfo> {
        self.inner.data.borrow().native_catalog.clone()
    }

    /// Make this cell the active one and put keyboard focus on its chart surface.
    pub fn focus(&self) {
        if let Some(obs) = self.observer() {
            obs.activate(&self.inner.id);
        }
        if let Some(chart) = self.try_chart() {
            chart.renderer().focus();
        }
    }

    // ── market ──

    /// Project a market identity into the cell state WITHOUT the data-dependent
    /// bookkeeping. Runs twice per user pick: once optimistically from the cell setters
    /// (the labels reflect the pick immediately, not after the bars load) and again from
    /// `market:changed` (the committed pass, and the only pass for host `set_market`
    /// calls). Idempotent, so the double run converges.
    fn project_market(&self, symbol: &str, timeframe: &str) {
        let session = self.try_chart().map(|c| c.market().session).unwrap_or(None);
        let mut d = self.inner.data.borrow_mut();
        // Any market other than the fallback's ends it: a host switching the chart
        // directly included.
        if let Some((fs, ft)) = &d.fallback
            && (fs != symbol || ft != timeframe)
        {
            d.fallback = None;
        }
        d.state.symbol = Some(symbol.to_string());
        d.state.provider = crate::data::provider_registry::parse_symbol(symbol).provider;
        if d.fallback.is_none() {
            d.state.timeframe = Some(timeframe.to_string());
        }
        d.state.session = session;
    }

    /// Switch this cell's shown session in place (a reload: RTH and ETH are different
    /// bars).
    pub fn set_session(&self, session: MarketSession) {
        if session == self.session() {
            return;
        }
        self.inner.data.borrow_mut().state.session = Some(session);
        self.state_dirty();
        if let Some(chart) = self.try_chart() {
            start_market(
                &chart,
                MarketSwitch {
                    session: Some(session),
                    ..MarketSwitch::default()
                },
            );
        }
    }

    /// Set the bars-to-fetch budget (the settings dialog's Advanced group): persisted, and
    /// the chart reloads at the deeper of it and the active range chip's depth.
    pub fn set_bars(&self, bars: usize) {
        let range_bars = {
            let mut d = self.inner.data.borrow_mut();
            d.state.bars = Some(bars as f64);
            d.range_bars
        };
        self.state_dirty();
        if let Some(chart) = self.try_chart() {
            start_market(
                &chart,
                MarketSwitch {
                    bars: Some(bars.max(range_bars)),
                    ..MarketSwitch::default()
                },
            );
        }
    }

    /// Push the resolved zone to the renderer: on the workspace choice changing, and on
    /// this cell's market zone changing under the exchange rule. Skips the write when the
    /// renderer already holds it (its config default is the bare `UTC` alias).
    pub fn apply_timezone(&self) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let zone = self.display_timezone();
        let current = chart.renderer().get("timezone");
        // A renderer without the feature reads `None`: treat it as the default so a UTC
        // workspace never issues a write it would only warn about.
        let held = match current.as_ref().and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => "UTC".to_string(),
        };
        if normalize_timezone(&held) == normalize_timezone(&zone) {
            return;
        }
        chart.renderer().set("timezone", json!(zone));
    }

    /// Re-read the symbol's metadata: session posture (RTH/ETH toggle) and its trading
    /// zone (the exchange rule). Async: the workspace re-projects when a verdict lands.
    fn refresh_symbol_metadata(&self, symbol: Option<String>, retry: bool) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let Some(symbol) = symbol.or_else(|| self.inner.data.borrow().state.symbol.clone()) else {
            return;
        };
        // Last ask wins: a slow probe must not overwrite a newer verdict.
        self.inner.data.borrow_mut().metadata_probe_symbol = Some(symbol.clone());
        let info = chart.data().symbol_info(&symbol);
        let weak = self.downgrade();
        let platform = self.inner.deps.env.platform.clone();
        let probe = symbol.clone();
        platform.spawn(Box::pin(async move {
            let si = info.await.ok().flatten();
            let Some(cell) = weak.upgrade() else { return };
            cell.on_symbol_info(chart, probe, si, retry);
        }));
    }

    fn on_symbol_info(&self, chart: Vela, symbol: String, si: Option<SymbolInfo>, retry: bool) {
        if self.inner.destroyed.get() {
            return;
        }
        let still = self
            .try_chart()
            .is_some_and(|c| c.market().symbol == chart.market().symbol);
        if !still || self.inner.data.borrow().metadata_probe_symbol.as_deref() != Some(&symbol) {
            return;
        }
        if si.is_none() && retry {
            // Nothing resolved the symbol yet: the EARLY pass can precede the provider
            // indexes. Ask again once they settle, then give up (the `market:changed` pass
            // is the backstop).
            let ready = chart.data().ready();
            let weak = self.downgrade();
            self.inner.deps.env.platform.spawn(Box::pin(async move {
                ready.await;
                if let Some(cell) = weak.upgrade() {
                    let current = cell.inner.data.borrow().metadata_probe_symbol.clone();
                    if current.as_deref() == Some(symbol.as_str()) {
                        cell.refresh_symbol_metadata(Some(symbol), false);
                    }
                }
            }));
            return;
        }
        // The cell's own state may not carry this symbol yet (host code calling
        // `set_market` directly projects it only at `market:changed`; that pass follows).
        if self.inner.data.borrow().state.symbol.as_deref() != Some(&symbol) {
            return;
        }
        let available = si
            .as_ref()
            .and_then(|i| i.session.as_deref())
            .is_some_and(|s| !s.is_empty() && s != "24x7");
        let overnight = si.as_ref().is_some_and(session_overnight);
        let zone = si
            .as_ref()
            .and_then(|i| i.timezone.clone())
            .filter(|z| !z.is_empty());
        let (zone_changed, flags_changed) = {
            let mut d = self.inner.data.borrow_mut();
            let zone_changed = zone != d.exchange_zone;
            if zone_changed {
                d.exchange_zone = zone;
            }
            let flags_changed = available != d.session_available
                || overnight != d.session_overnight
                || zone_changed;
            if flags_changed {
                d.session_available = available;
                d.session_overnight = overnight;
            }
            (zone_changed, flags_changed)
        };
        if zone_changed {
            self.apply_timezone(); // a no-op unless the workspace follows the exchange
        }
        if flags_changed && let Some(obs) = self.observer() {
            obs.market_changed(&self.inner.id); // re-project the shared bottombar toggle and zone label
        }
    }

    /// The session-shade colors live in the renderer CONFIG (persisted with it, edited
    /// live by the dialog swatch): the cell only proxies them into its settings rows.
    /// `key` is `premarketColor`, `postmarketColor` or `extendedColor`.
    pub fn session_shade_color(&self, key: &str) -> String {
        self.try_chart()
            .and_then(|c| c.renderer().get_config())
            .and_then(|cfg| cfg.get("sessions")?.get(key)?.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    pub fn set_session_shade_color(&self, key: &str, color: &str) {
        if let Some(chart) = self.try_chart() {
            chart
                .renderer()
                .apply_config(&json!({ "sessions": { key: color } }));
        }
        self.state_dirty(); // the colors persist with the renderer config document
    }

    /// Switch this cell's market symbol in place (the chart instance survives). The
    /// projection is OPTIMISTIC: labels and chrome show the pick before the bars load; it
    /// follows the `set_market` call.
    pub fn set_symbol(&self, symbol: &str) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        if symbol == self.symbol() {
            return;
        }
        // A fallback belongs to the symbol that refused: the next one is asked for the
        // requested timeframe again (so clear it BEFORE reading the timeframe).
        let fell_back = {
            let mut d = self.inner.data.borrow_mut();
            d.unresolved_toasted = None; // a re-picked symbol gets a fresh verdict
            let fell_back = d.fallback.is_some();
            d.fallback = None;
            fell_back
        };
        let timeframe = self.timeframe();
        start_market(
            &chart,
            if fell_back {
                MarketSwitch {
                    symbol: Some(symbol.to_string()),
                    timeframe: Some(timeframe.clone()),
                    ..MarketSwitch::default()
                }
            } else {
                MarketSwitch {
                    symbol: Some(symbol.to_string()),
                    ..MarketSwitch::default()
                }
            },
        );
        self.project_market(symbol, &timeframe);
        if let Some(obs) = self.observer() {
            obs.market_changed(&self.inner.id);
        }
    }

    /// An explicit timeframe choice: it becomes the requested one, also while a fallback
    /// runs: picking the fallback keeps it shown, anything else asks the chart for it.
    pub fn set_timeframe(&self, timeframe: &str) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let has_fallback = self.inner.data.borrow().fallback.is_some();
        if !has_fallback && timeframe == self.timeframe() {
            return;
        }
        let shown = self.timeframe();
        self.inner.data.borrow_mut().fallback = None;
        if timeframe == shown {
            self.project_market(&self.symbol(), timeframe);
            if let Some(obs) = self.observer() {
                obs.market_changed(&self.inner.id);
            }
            return;
        }
        // Leaving range mode: drop the chip AND its fetch budget (back to the cell's own bars).
        let bars = {
            let mut d = self.inner.data.borrow_mut();
            d.active_range_id = None;
            d.range_bars = 0;
            d.state.bars
        };
        start_market(
            &chart,
            MarketSwitch {
                timeframe: Some(timeframe.to_string()),
                bars: bars.map(|b| b as usize),
                ..MarketSwitch::default()
            },
        );
        self.project_market(&self.symbol(), timeframe);
        if let Some(obs) = self.observer() {
            obs.market_changed(&self.inner.id);
        }
    }

    /// The source refused the chart's timeframe for this symbol for good: switch to the one
    /// it suggests (`default`, else `finest`) and say so, keeping the requested timeframe
    /// as the cell's own. A refusal of the fallback itself, or one with nothing better to
    /// offer, is only reported: the cell never chains switches.
    fn on_data_unavailable(&self, ev: &ChartEvent) {
        let ChartEvent::DataUnavailable {
            symbol: e_symbol,
            timeframe: e_tf,
            default,
            finest,
            reason,
        } = ev
        else {
            return;
        };
        let _: &DataUnavailableReason = reason;
        let Some(chart) = self.try_chart() else {
            return;
        };
        if self.inner.destroyed.get() {
            return;
        }
        let market = chart.market();
        let current_symbol = market.symbol.clone().unwrap_or_else(|| "TEST".into());
        let current_tf = market.timeframe.clone().unwrap_or_else(|| "60".into());
        if *e_symbol != current_symbol || *e_tf != current_tf {
            return; // superseded
        }
        let i18n = chart.i18n();
        let i18n_ref: &dyn Translator = i18n.as_ref();
        let parsed = crate::data::provider_registry::parse_symbol(e_symbol);
        let symbol = if parsed.ticker.is_empty() {
            e_symbol.clone()
        } else {
            parsed.ticker
        };
        let asked = timeframe_label(e_tf, Some(i18n_ref));
        let to = chart_timeframe_of(default.as_deref())
            .or_else(|| chart_timeframe_of(finest.as_deref()));
        let again = self
            .inner
            .data
            .borrow()
            .fallback
            .as_ref()
            .is_some_and(|(s, t)| s == e_symbol && t == e_tf);
        let same = to
            .as_deref()
            .is_some_and(|to| timeframe_ms(to) == timeframe_ms(e_tf));
        let Some(to) = to.filter(|_| !again && !same) else {
            let text = text_of(
                Some(i18n_ref),
                &Text::unavailable(),
                &[
                    ("symbol", MessageArg::from(symbol)),
                    ("timeframe", MessageArg::from(asked)),
                ],
            );
            self.toast(&text, ToastKind::Error, 6000.0);
            return;
        };
        let bars = {
            let mut d = self.inner.data.borrow_mut();
            d.fallback = Some((e_symbol.clone(), to.clone()));
            // The range chip's timeframe is not what shows any more: leave range mode.
            d.active_range_id = None;
            d.range_bars = 0;
            d.pending_range = None;
            d.state.bars
        };
        start_market(
            &chart,
            MarketSwitch {
                timeframe: Some(to.clone()),
                bars: bars.map(|b| b as usize),
                ..MarketSwitch::default()
            },
        );
        self.project_market(e_symbol, &to);
        if let Some(obs) = self.observer() {
            obs.market_changed(&self.inner.id);
        }
        let text = text_of(
            Some(i18n_ref),
            &Text::unavailable_fallback(),
            &[
                ("symbol", MessageArg::from(symbol)),
                ("timeframe", MessageArg::from(asked)),
                (
                    "shown",
                    MessageArg::from(timeframe_label(&to, Some(i18n_ref))),
                ),
            ],
        );
        self.toast(&text, ToastKind::Info, 5000.0);
    }

    /// Applied live (renderer feature): no reload.
    pub fn set_price_style(&self, style: &str) {
        self.inner.data.borrow_mut().state.price_style = Some(style.to_string());
        if let Some(chart) = self.try_chart() {
            chart.renderer().set("priceStyle", json!(style));
        }
        if let Some(obs) = self.observer() {
            obs.price_style_changed(&self.inner.id);
        }
    }

    /// Frame a bottombar range chip: switch to its timeframe, fetch the depth its window
    /// needs, and keep it framed (re-asserted once the deeper history is painted).
    pub fn apply_range(&self, preset: &CellRangePreset) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        if self.inner.destroyed.get() {
            return;
        }
        let shown = self.timeframe();
        // A chip names its timeframe explicitly: a running fallback ends, the chip's
        // timeframe is the requested one from here on.
        let ends_fallback = {
            let mut d = self.inner.data.borrow_mut();
            let ends = d.fallback.is_some();
            if ends {
                d.fallback = None;
                d.state.timeframe = Some(preset.tf.clone());
            }
            d.active_range_id = Some(preset.id.clone());
            ends
        };
        let (deeper, bars) = {
            let mut d = self.inner.data.borrow_mut();
            let base = d.state.bars.map_or(500, |b| b as usize);
            let deeper = preset.bars > base.max(d.range_bars);
            d.range_bars = preset.bars;
            (deeper, base.max(d.range_bars))
        };
        if preset.tf != shown || deeper {
            self.inner.data.borrow_mut().pending_range = Some(preset.clone());
            let load = chart.set_market(MarketSwitch {
                timeframe: Some(preset.tf.clone()),
                bars: Some(bars),
                visible_range: Some(InitialRange::Preset(preset.preset)),
                ..MarketSwitch::default()
            });
            let weak = self.downgrade();
            let preset = preset.clone();
            self.inner.deps.env.platform.spawn(Box::pin(async move {
                let _ = load.await;
                let Some(cell) = weak.upgrade() else { return };
                if cell.inner.destroyed.get() {
                    return;
                }
                let pending = cell.inner.data.borrow().pending_range.clone();
                if pending.as_ref() == Some(&preset) {
                    if let Some(chart) = cell.try_chart() {
                        chart.set_visible_range_preset(preset.preset);
                    }
                    cell.inner.data.borrow_mut().pending_range = None;
                }
            }));
        } else {
            chart.set_visible_range_preset(preset.preset);
        }
        if ends_fallback && let Some(obs) = self.observer() {
            obs.market_changed(&self.inner.id); // the requested timeframe changed
        }
    }

    /// Reset this cell's view: re-enable auto scale and frame the full history.
    pub fn reset_view(&self) {
        if let Some(chart) = self.try_chart() {
            chart.renderer().set("autoScale", json!(true));
            chart.set_visible_range_preset(VisibleRangePreset::All);
        }
    }

    // ── presentation prefs ──

    /// Show or hide this cell's symbol watermark (persisted per cell).
    pub fn set_watermark_visible(&self, visible: bool) {
        self.inner.data.borrow_mut().watermark_on = visible;
        self.prefs_changed();
    }

    /// Show or hide the "Replay" line under this cell's watermark while it replays.
    pub fn set_replay_watermark_visible(&self, visible: bool) {
        self.inner.data.borrow_mut().replay_watermark_on = visible;
        self.prefs_changed();
    }

    /// Show or hide this cell's indicator titles (the in-chart legend rows).
    pub fn set_indicator_titles_visible(&self, visible: bool) {
        self.inner.data.borrow_mut().indicator_titles_on = visible;
        if let Some(chart) = self.try_chart() {
            chart.renderer().set("indicatorTitles", json!(visible));
        }
        self.prefs_changed();
    }

    /// Show or hide the plot values beside this cell's legend titles.
    pub fn set_indicator_values_visible(&self, visible: bool) {
        self.inner.data.borrow_mut().indicator_values_on = visible;
        if let Some(chart) = self.try_chart() {
            chart.renderer().set("indicatorValues", json!(visible));
        }
        self.prefs_changed();
    }

    /// Show or hide one status-line segment (the settings dialog's Status line tab).
    pub fn set_statusline_part(&self, part: StatuslinePart, visible: bool) {
        {
            let mut d = self.inner.data.borrow_mut();
            let Some(parts) = d.statusline_parts.as_mut() else {
                return;
            };
            parts.set(part, visible);
        }
        self.prefs_changed();
    }

    fn prefs_changed(&self) {
        self.state_dirty();
        if let Some(obs) = self.observer() {
            obs.status_prefs_changed(&self.inner.id);
        }
    }

    /// This cell's Status line tab and watermark prefs as one bundle.
    pub fn status_prefs(&self) -> CellStatusPrefs {
        let d = self.inner.data.borrow();
        CellStatusPrefs {
            parts: d.statusline_parts,
            indicator_titles: d.indicator_titles_on,
            indicator_values: d.indicator_values_on,
            watermark: d.watermark_on,
            replay_watermark: d.replay_watermark_on,
        }
    }

    /// Converge this cell's Status line tab and watermark prefs to `prefs`: the follower
    /// half of the workspace's style link. Idempotent: matching values change nothing, so
    /// a propagated echo dies on its own.
    pub fn apply_status_prefs(&self, prefs: &CellStatusPrefs) {
        let have = self.status_prefs();
        if let (Some(want), Some(held)) = (prefs.parts, have.parts) {
            for part in StatuslinePart::ALL {
                if held.get(part) != want.get(part) {
                    self.set_statusline_part(part, want.get(part));
                }
            }
        }
        if prefs.indicator_titles != have.indicator_titles {
            self.set_indicator_titles_visible(prefs.indicator_titles);
        }
        if prefs.indicator_values != have.indicator_values {
            self.set_indicator_values_visible(prefs.indicator_values);
        }
        if prefs.watermark != have.watermark {
            self.set_watermark_visible(prefs.watermark);
        }
        if prefs.replay_watermark != have.replay_watermark {
            self.set_replay_watermark_visible(prefs.replay_watermark);
        }
    }

    /// Re-read the cell's own chrome in the chart's language, after a locale change: the
    /// native catalog's titles. (The DOM chrome relabels itself from the chart's
    /// `locale:changed` event.)
    pub fn relocalize(&self) {
        if self.try_chart().is_some() {
            self.refresh_native_catalog();
        }
    }

    // ── indicator ledger (shared manifest, per-cell instances) ──

    /// Hand the cell the workspace's resolved manifest. A RESTORED ledger (pool or
    /// persisted state) re-adds its recorded entries by name, held until the manifest
    /// actually carries them. Otherwise `seed_enabled` auto-adds the manifest's `enabled`
    /// entries (fresh cells only).
    pub fn set_manifest(&self, list: &[ManifestIndicator], seed_enabled: bool) {
        let pending = {
            let mut d = self.inner.data.borrow_mut();
            d.manifest = list.to_vec();
            d.pending_manifest.clone()
        };
        if let Some(pending) = pending {
            if list.is_empty() {
                return; // the manifest has not resolved yet: keep waiting
            }
            for led in &pending {
                if let Some(entry) = list.iter().find(|e| e.name == ledger_entry_name(led)) {
                    let opts = ManifestAddOptions::from_ledger(led);
                    self.add_manifest_instance(entry.clone(), opts);
                }
            }
            self.inner.data.borrow_mut().pending_manifest = None;
            return;
        }
        if seed_enabled {
            for entry in list.iter().filter(|e| e.enabled) {
                self.add_manifest_instance(
                    entry.clone(),
                    ManifestAddOptions {
                        record: false,
                        ..ManifestAddOptions::default()
                    },
                );
            }
        }
    }

    /// Replace the indicator ledger: natives converge to the listed set (volume included:
    /// removing it sticks, the core's auto-add respects the opt-out), and manifest
    /// instances are re-created by name, held until the shared manifest resolves.
    /// Convergence is state application, not user edits: nothing enters the undo timeline.
    fn apply_indicator_ledger(&self, led: &IndicatorsState) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        self.inner.data.borrow_mut().volume_intent = led
            .natives
            .iter()
            .any(|e| ledger_native_type(e) == "volume");
        self.inner.history.silently(|| {
            // Converge as a MULTISET: a multi-instance type is listed once per instance.
            // Existing instances are kept while the ledger still owes their type (each
            // consumes one owed ENTRY and converges to its values and visibility), the
            // surplus goes, the shortfall is added with the entry's values.
            let mut owed: Vec<(String, Vec<&PersistedNativeEntry>)> = Vec::new();
            for e in &led.natives {
                let t = ledger_native_type(e).to_string();
                match owed.iter_mut().find(|(k, _)| *k == t) {
                    Some((_, q)) => q.push(e),
                    None => owed.push((t, vec![e])),
                }
            }
            for h in self.native_handles() {
                let t = h.native_type().unwrap_or_default().to_string();
                let entry = owed
                    .iter_mut()
                    .find(|(k, _)| *k == t)
                    .and_then(|(_, q)| (!q.is_empty()).then(|| q.remove(0)));
                match entry {
                    Some(entry) => Self::apply_native_ledger_entry(h.as_ref(), entry),
                    None => h.remove(),
                }
            }
            for (_, queue) in owed {
                for e in queue {
                    let h = chart.add_native_indicator(
                        ledger_native_type(e),
                        NativeIndicatorOptions::default(),
                    );
                    Self::apply_native_ledger_entry(h.as_ref(), e);
                }
            }
            let instances: Vec<Rc<CellInstance>> = self.inner.data.borrow().instances.clone();
            for it in instances {
                self.drop_instance(&it);
            }
            let manifest = self.inner.data.borrow().manifest.clone();
            if !manifest.is_empty() {
                for item in &led.manifest {
                    if let Some(entry) = manifest.iter().find(|e| e.name == ledger_entry_name(item))
                    {
                        self.add_manifest_instance(
                            entry.clone(),
                            ManifestAddOptions::from_ledger(item),
                        );
                    }
                }
                self.inner.data.borrow_mut().pending_manifest = None;
            } else if !self.observer().is_some_and(|o| o.manifest_settled()) {
                self.inner.data.borrow_mut().pending_manifest = Some(led.manifest.clone());
            } else {
                self.inner.data.borrow_mut().pending_manifest = None; // no manifest will ever resolve
            }
        });
        self.sync_present_natives();
        self.refresh_native_catalog();
    }

    /// The supported natives in picker order: A to Z by title (registration order follows
    /// the catalog's families, which is meaningless to the reader). This is the library
    /// index space the picker hands back, so [`library_rows`](Self::library_rows) and
    /// [`add_from_library`](Self::add_from_library) both read it.
    fn supported_natives(&self) -> Vec<CellNativeInfo> {
        let mut out: Vec<CellNativeInfo> = self
            .inner
            .data
            .borrow()
            .native_catalog
            .iter()
            .filter(|n| n.supported)
            .cloned()
            .collect();
        // Titles are in the chart's language; compare case-insensitively (the original
        // sorts with the locale's base-sensitivity collator).
        out.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.title.cmp(&b.title))
        });
        out
    }

    /// The picker's library rows: supported natives first, then the manifest in the host's
    /// own order.
    pub fn library_rows(&self) -> Vec<LibraryRow> {
        let i18n = self.try_chart().map(|c| c.i18n());
        let i18n_ref = i18n.as_deref();
        let category = text_of(i18n_ref, &Text::built_in(), &[]);
        let mut rows: Vec<LibraryRow> = self
            .supported_natives()
            .into_iter()
            .map(|n| LibraryRow {
                aliases: native_aliases(&n.indicator_type),
                name: n.title,
                language: None,
                category: Some(category.clone()),
                native: true,
                native_type: Some(n.indicator_type),
                beta: n.beta,
            })
            .collect();
        rows.extend(
            self.inner
                .data
                .borrow()
                .manifest
                .iter()
                .map(|e| LibraryRow {
                    name: e.name.clone(),
                    aliases: Vec::new(),
                    language: e.language.clone(),
                    category: e.category.clone(),
                    native: false,
                    native_type: None,
                    beta: false,
                }),
        );
        rows
    }

    /// The picker's on-chart rows: native instances first, then live script instances.
    pub fn on_chart_rows(&self) -> Vec<OnChartRow> {
        let mut rows: Vec<OnChartRow> = self
            .native_handles()
            .iter()
            .map(|h| OnChartRow {
                name: h.title(),
                aliases: h.native_type().map(native_aliases).unwrap_or_default(),
                language: None,
                native: true,
                native_type: h.native_type().map(str::to_string),
            })
            .collect();
        rows.extend(
            self.inner
                .data
                .borrow()
                .instances
                .iter()
                .map(|it| OnChartRow {
                    name: it.entry.name.clone(),
                    aliases: Vec::new(),
                    language: it.entry.language.clone(),
                    native: false,
                    native_type: None,
                }),
        );
        rows
    }

    /// Add by picker LIBRARY index (natives precede the manifest: mirrors `library_rows`).
    pub fn add_from_library(&self, index: usize) {
        let natives = self.supported_natives();
        if let Some(n) = natives.get(index) {
            self.add_native(&n.indicator_type);
        } else {
            let entry = self
                .inner
                .data
                .borrow()
                .manifest
                .get(index - natives.len())
                .cloned();
            if let Some(entry) = entry {
                self.add_manifest_instance(entry, ManifestAddOptions::default());
            }
        }
    }

    /// Remove by picker ON-CHART index (native instances precede script instances).
    pub fn remove_from_chart(&self, index: usize) {
        let natives = self.native_handles();
        if let Some(h) = natives.get(index) {
            // The undo entry is recorded by the indicator:removed handler: the single site
            // shared with the legend close button and the object tree.
            h.remove();
            self.refresh_native_catalog();
        } else {
            self.remove_instance(index - natives.len());
        }
    }

    /// Add a script indicator through the PUBLIC seam (`ctx.addIndicator`): same undo/redo
    /// and picker plumbing as a manifest entry, but flagged `external` so the persisted
    /// ledger never records a name the manifest cannot resolve (the plugin owns persistence
    /// through its state handler). Recording follows the ambient mute.
    pub fn add_external_indicator(&self, entry: ExternalIndicatorEntry) {
        let ExternalIndicatorEntry {
            name,
            script,
            language,
            category,
            id,
            inputs,
            props,
            hidden,
        } = entry;
        self.add_manifest_instance(
            ManifestIndicator {
                name,
                script,
                language,
                category,
                enabled: true,
            },
            ManifestAddOptions {
                record: true,
                external: true,
                id,
                inputs,
                props,
                hidden,
            },
        );
    }

    /// Add ONE instance of a manifest entry (repeatable: duplicates are legitimate).
    pub fn add_manifest_instance(&self, entry: ManifestIndicator, opts: ManifestAddOptions) {
        if self.inner.destroyed.get() {
            return;
        }
        let values = (opts.inputs.is_some() || opts.props.is_some()).then(|| InstanceValues {
            inputs: opts.inputs.clone(),
            props: opts.props.clone(),
        });
        let Some(handle) = self.add_to_chart(&entry, values.as_ref(), opts.id.as_deref()) else {
            // The chart refused the add (a host id already live on it): no instance, no
            // history entry.
            return;
        };
        let it = Rc::new(CellInstance {
            entry,
            id: RefCell::new(Some(handle.id().to_string())),
            handle: RefCell::new(Some(handle)),
            external: opts.external,
            values: RefCell::new(values),
        });
        // A restored `hidden` applies right after the add: hiding suspends the engine
        // session before it spends anything on an indicator the user had tucked away.
        if opts.hidden
            && let Some(h) = it.handle.borrow().as_ref()
        {
            h.set_visible(false);
        }
        self.inner.data.borrow_mut().instances.push(it.clone());
        self.indicators_changed();
        if !opts.record {
            return;
        }
        let (undo_cell, redo_cell) = (self.downgrade(), self.downgrade());
        let (undo_it, redo_it) = (it.clone(), it);
        self.inner.history.push(HistoryAction::new(
            move || {
                if let Some(c) = undo_cell.upgrade() {
                    c.drop_instance(&undo_it);
                }
            },
            move || {
                if let Some(c) = redo_cell.upgrade() {
                    c.revive_instance(&redo_it);
                }
            },
        ));
    }

    /// Re-add a dropped instance under its recorded id and values (undo and redo
    /// resurrection).
    fn revive_instance(&self, it: &Rc<CellInstance>) {
        let id = it.id.borrow().clone();
        let values = it.values.borrow().clone();
        *it.handle.borrow_mut() = self.add_to_chart(&it.entry, values.as_ref(), id.as_deref());
        self.inner.data.borrow_mut().instances.push(it.clone());
        self.indicators_changed();
    }

    fn indicators_changed(&self) {
        if let Some(obs) = self.observer() {
            obs.indicators_changed(&self.inner.id);
        }
    }

    fn remove_instance(&self, index: usize) {
        let it = self.inner.data.borrow().instances.get(index).cloned();
        let Some(it) = it else { return };
        if self.inner.destroyed.get() {
            return;
        }
        self.drop_instance(&it);
        let (undo_cell, redo_cell) = (self.downgrade(), self.downgrade());
        let (undo_it, redo_it) = (it.clone(), it);
        self.inner.history.push(HistoryAction::new(
            move || {
                if let Some(c) = undo_cell.upgrade() {
                    c.revive_instance(&undo_it);
                }
            },
            move || {
                if let Some(c) = redo_cell.upgrade() {
                    c.drop_instance(&redo_it);
                }
            },
        ));
    }

    fn drop_instance(&self, it: &Rc<CellInstance>) {
        self.inner
            .data
            .borrow_mut()
            .instances
            .retain(|other| !Rc::ptr_eq(other, it));
        // Capture the deltas BEFORE removal: an undo/redo resurrection re-adds the
        // indicator with the values the user last saw, not the declaration defaults.
        let handle = it.handle.borrow_mut().take();
        if let Some(handle) = handle {
            *it.values.borrow_mut() = instance_deltas(handle.as_ref());
            handle.remove();
        }
        self.indicators_changed();
    }

    /// `indicator:removed`: out-of-band removals (legend close, object tree, middle click,
    /// `handle.remove()`) must drop the matching manifest-instance ledger entry too, AND
    /// enter the undo timeline like a picker removal would. The picker path splices and
    /// records first (so these lookups no-op there), and replays run muted.
    fn on_indicator_removed(&self, id: &str) {
        if self.inner.destroyed.get() {
            return;
        }
        let found = self
            .inner
            .data
            .borrow()
            .instances
            .iter()
            .find(|it| it.handle.borrow().as_ref().is_some_and(|h| h.id() == id))
            .cloned();
        if let Some(snapshot) = found {
            self.inner
                .data
                .borrow_mut()
                .instances
                .retain(|other| !Rc::ptr_eq(other, &snapshot));
            let (undo_cell, redo_cell) = (self.downgrade(), self.downgrade());
            let (undo_it, redo_it) = (snapshot.clone(), snapshot);
            self.inner.history.push(HistoryAction::new(
                move || {
                    if let Some(c) = undo_cell.upgrade() {
                        c.revive_instance(&undo_it);
                    }
                },
                move || {
                    if let Some(c) = redo_cell.upgrade() {
                        c.drop_instance(&redo_it);
                    }
                },
            ));
        } else {
            // A native indicator left the registry: the sync mirror still knows its type,
            // which is what an undo must re-add. This is the SINGLE recording site for
            // native removals. The redo removes the instance the undo created, never "the
            // one of that type": a multi-instance type may have siblings on the chart.
            let gone = self
                .inner
                .data
                .borrow()
                .present_natives
                .iter()
                .find(|(nid, _)| nid == id)
                .map(|(_, t)| t.clone());
            if let Some(kind) = gone {
                let revived: Rc<RefCell<Option<IndicatorHandleRef>>> = Rc::default();
                let (undo_cell, redo_cell) = (self.downgrade(), self.downgrade());
                let (undo_slot, redo_slot) = (revived.clone(), revived);
                self.inner.history.push(HistoryAction::new(
                    move || {
                        if let Some(c) = undo_cell.upgrade() {
                            let h = c.try_chart().map(|chart| {
                                chart.add_native_indicator(&kind, NativeIndicatorOptions::default())
                            });
                            *undo_slot.borrow_mut() = h;
                            c.refresh_native_catalog();
                        }
                    },
                    move || {
                        if let Some(c) = redo_cell.upgrade() {
                            if let Some(h) = redo_slot.borrow_mut().take() {
                                h.remove();
                            }
                            c.refresh_native_catalog();
                        }
                    },
                ));
            }
        }
        self.sync_present_natives();
        self.refresh_native_catalog();
    }

    /// Converge one native handle to a restored ledger entry: every schema key gets the
    /// entry's value or its declaration default (the DOCUMENT is the truth: a live tweak
    /// must not survive a restore that says otherwise), and visibility follows the `hidden`
    /// flag. Skips the input write when nothing differs: `set_inputs` re-runs the
    /// indicator, and a no-op restore must not flicker every native.
    fn apply_native_ledger_entry(handle: &dyn IndicatorHandle, entry: &PersistedNativeEntry) {
        let (wanted, hidden) = match entry {
            PersistedNativeEntry::Type(_) => (None, false),
            PersistedNativeEntry::Entry { inputs, hidden, .. } => {
                (inputs.as_ref().map(map_to_inputs), hidden.unwrap_or(false))
            }
        };
        let schema: Vec<InputSchema> = handle.inputs();
        if !schema.is_empty() {
            let current = handle.input_values();
            let mut next = Inputs::new();
            for s in &schema {
                let target = wanted
                    .as_ref()
                    .and_then(|w| w.get(&s.key))
                    .cloned()
                    .unwrap_or_else(|| s.defval.clone());
                if current.get(&s.key) != Some(&target) {
                    next.insert(s.key.clone(), target);
                }
            }
            if !next.is_empty() {
                handle.set_inputs(next);
            }
        } else if let Some(wanted) = wanted {
            // Schema not exposed (nothing to diff against): apply the stored values verbatim.
            handle.set_inputs(wanted);
        }
        let visible = !hidden;
        if handle.visible() != visible {
            handle.set_visible(visible);
        }
    }

    /// Add a native indicator. A multi-instance type gets a fresh instance every time; a
    /// single-instance type already on the chart hands back its existing one: nothing
    /// changed, so nothing enters the undo timeline.
    pub fn add_native(&self, indicator_type: &str) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let before: Vec<String> = chart
            .indicators()
            .iter()
            .map(|h| h.id().to_string())
            .collect();
        let added = chart.add_native_indicator(indicator_type, NativeIndicatorOptions::default());
        self.sync_present_natives();
        self.refresh_native_catalog();
        if before.iter().any(|b| b == added.id()) {
            return;
        }
        let slot: Rc<RefCell<Option<IndicatorHandleRef>>> = Rc::new(RefCell::new(Some(added)));
        let (undo_cell, redo_cell) = (self.downgrade(), self.downgrade());
        let (undo_slot, redo_slot) = (slot.clone(), slot);
        let (undo_type, redo_type) = (indicator_type.to_string(), indicator_type.to_string());
        let _ = undo_type;
        self.inner.history.push(HistoryAction::new(
            move || {
                if let Some(h) = undo_slot.borrow_mut().take() {
                    h.remove();
                }
                if let Some(c) = undo_cell.upgrade() {
                    c.refresh_native_catalog();
                }
            },
            move || {
                if let Some(c) = redo_cell.upgrade() {
                    let h = c.try_chart().map(|chart| {
                        chart.add_native_indicator(&redo_type, NativeIndicatorOptions::default())
                    });
                    *redo_slot.borrow_mut() = h;
                    c.refresh_native_catalog();
                }
            },
        ));
    }

    /// The chart's native instances, insertion order: the picker's on-chart rows and the
    /// removal index space (script instances follow them).
    fn native_handles(&self) -> Vec<IndicatorHandleRef> {
        self.try_chart()
            .map(|c| {
                c.indicators()
                    .into_iter()
                    .filter(|h| h.native_type().is_some())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sync mirror of the chart's native instances: the removal handler looks the removed
    /// id up here (the registry has already forgotten it) to record its type.
    fn sync_present_natives(&self) {
        let list: Vec<(String, String)> = self
            .native_handles()
            .iter()
            .map(|h| {
                (
                    h.id().to_string(),
                    h.native_type().unwrap_or_default().to_string(),
                )
            })
            .collect();
        self.inner.data.borrow_mut().present_natives = list;
    }

    /// Refresh the native catalog (supported/present flags) for this cell's market.
    pub fn refresh_native_catalog(&self) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        let list = chart.available_native_indicators();
        let weak = self.downgrade();
        self.inner.deps.env.platform.spawn(Box::pin(async move {
            let list = list.await;
            let Some(cell) = weak.upgrade() else { return };
            if cell.inner.destroyed.get() || cell.try_chart().is_none() {
                return;
            }
            cell.inner.data.borrow_mut().native_catalog = list
                .into_iter()
                .map(|n| CellNativeInfo {
                    indicator_type: n.indicator_type,
                    title: n.title,
                    supported: n.supported,
                    present: n.present,
                    beta: n.beta,
                })
                .collect();
            if let Some(obs) = cell.observer() {
                obs.catalog_changed(&cell.inner.id);
            }
        }));
    }

    fn add_to_chart(
        &self,
        entry: &ManifestIndicator,
        values: Option<&InstanceValues>,
        id: Option<&str>,
    ) -> Option<IndicatorHandleRef> {
        let chart = self.try_chart()?;
        let options = AddIndicatorOptions {
            id: id.map(str::to_string),
            language: entry.language.clone(),
            inputs: values.and_then(|v| v.inputs.clone()),
            props: values.and_then(|v| v.props.clone()),
            ..AddIndicatorOptions::default()
        };
        match chart.add_indicator(&entry.script, options) {
            Ok(handle) => Some(handle),
            Err(err) => {
                self.inner.deps.env.platform.warn(&format!(
                    "[vela] indicator \"{}\" failed to add: {err}",
                    entry.name
                ));
                None
            }
        }
    }

    // ── third-party state (the `ext` seam) ──

    /// Run the registered cell-scope `restore` handlers against the cell's restored `ext`
    /// bag. The workspace calls this AFTER the core state is in place (chart alive and
    /// wired, indicator ledger converged). Muted: nothing a restore does enters the undo
    /// timeline. Handlers only see keys the document carries.
    pub fn restore_persisted_ext(&self) {
        if self.inner.destroyed.get() {
            return;
        }
        for h in &self.inner.deps.ext_handlers {
            let value = self.inner.data.borrow().ext_state.get(h.key()).cloned();
            if let Some(value) = value {
                self.inner.history.silently(|| h.restore(&value, self));
            }
        }
    }

    /// Assemble the cell's `ext` bag: fresh handler snapshots merged OVER the preserved
    /// entries: a key with no handler this session rides along verbatim; a registered
    /// handler returning `None` withdraws its entry.
    fn dehydrate_ext(&self) -> Option<BTreeMap<String, Value>> {
        let mut ext = self.inner.data.borrow().ext_state.clone();
        for h in &self.inner.deps.ext_handlers {
            match h.serialize(self) {
                Some(v) => {
                    ext.insert(h.key().to_string(), v);
                }
                None => {
                    ext.remove(h.key());
                }
            }
        }
        self.inner.data.borrow_mut().ext_state = ext.clone(); // the merged bag is the new baseline
        (!ext.is_empty()).then_some(ext)
    }

    // ── lifecycle ──

    /// Apply a restored cell state IN PLACE: the chart instance survives (the market
    /// switches via `set_market`) while cosmetics, renderer config, drawings and the
    /// indicator ledger converge to the document. The workspace takes this path when a
    /// state document lands on a grid of the same shape, so chart references, indicator
    /// handles, event subscriptions and the cell host all stay valid.
    pub fn rehydrate(&self, cs: &CellState) {
        let Some(chart) = self.try_chart() else {
            return;
        };
        if self.inner.destroyed.get() {
            return;
        }
        if let Some(style) = cs.price_style.as_deref().filter(|s| !s.is_empty())
            && style != self.requested_price_style()
        {
            self.set_price_style(style);
        }
        let have = self.status_prefs();
        if let Some(v) = cs.watermark
            && v != have.watermark
        {
            self.set_watermark_visible(v);
        }
        if let Some(v) = cs.replay_watermark
            && v != have.replay_watermark
        {
            self.set_replay_watermark_visible(v);
        }
        if let Some(v) = cs.indicator_titles
            && v != have.indicator_titles
        {
            self.set_indicator_titles_visible(v);
        }
        if let Some(v) = cs.indicator_values
            && v != have.indicator_values
        {
            self.set_indicator_values_visible(v);
        }
        if let (Some(saved), Some(held)) = (cs.statusline_parts, have.parts) {
            for (part, v) in [
                (StatuslinePart::Logo, saved.logo),
                (StatuslinePart::Name, saved.name),
                (StatuslinePart::Market, saved.market),
                (StatuslinePart::Ohlc, saved.ohlc),
                (StatuslinePart::Change, saved.change),
            ] {
                if let Some(v) = v
                    && held.get(part) != v
                {
                    self.set_statusline_part(part, v);
                }
            }
        }
        // Cosmetics + drawings round-trip (both validate untrusted input).
        if let Some(config) = &cs.renderer_config {
            chart.renderer().apply_config(config);
        }
        if let Some(doc) = &cs.drawings {
            chart.drawings().from_json(doc);
        }
        if let Some(ledger) = &cs.indicators {
            self.apply_indicator_ledger(ledger);
        }
        // Third-party state converges to the document too: the restored bag REPLACES the
        // baseline (absent in the document = the document carries none), then the
        // registered handlers re-apply. After the ledger.
        self.inner.data.borrow_mut().ext_state = cs.ext.clone().unwrap_or_default();
        self.restore_persisted_ext();
        // Market last, as ONE in-place switch: `market:changed` re-syncs the cell and
        // notifies the workspace.
        let symbol = prefixed_symbol(Some(cs));
        let session = session_of(cs.session.as_deref()).unwrap_or(MarketSession::Regular);
        let bars = match cs.bars {
            Some(b) if b.is_finite() && b > 0.0 => b,
            _ => 0.0,
        };
        let mut next = MarketSwitch::default();
        let mut changed = false;
        if let Some(symbol) = symbol.filter(|s| *s != self.symbol()) {
            next.symbol = Some(symbol);
            changed = true;
        }
        let timeframe = cs
            .timeframe
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.requested_timeframe());
        let fallback = self.inner.data.borrow().fallback.clone();
        if let Some((_, shown)) = fallback
            && (next.symbol.is_some() || timeframe != self.requested_timeframe())
        {
            // The document names another market or timeframe: the fallback ends, and the
            // document's timeframe is the requested one (asked for unless already shown).
            let mut d = self.inner.data.borrow_mut();
            d.fallback = None;
            d.state.timeframe = Some(timeframe.clone());
            drop(d);
            if timeframe != shown {
                next.timeframe = Some(timeframe.clone());
                changed = true;
            }
        } else if self.inner.data.borrow().fallback.is_none()
            && cs
                .timeframe
                .as_deref()
                .is_some_and(|t| !t.is_empty() && t != self.timeframe())
        {
            next.timeframe = cs.timeframe.clone();
            changed = true;
        }
        if session != self.session() {
            self.inner.data.borrow_mut().state.session = Some(session);
            next.session = Some(session);
            changed = true;
        }
        let range_bars = self.inner.data.borrow().range_bars;
        let held_bars = self.inner.data.borrow().state.bars;
        if bars > 0.0 && Some(bars) != held_bars {
            self.inner.data.borrow_mut().state.bars = Some(bars);
            next.bars = Some((bars as usize).max(range_bars));
            changed = true;
        }
        if changed {
            start_market(&chart, next);
        }
    }

    /// Snapshot everything the pool needs to restore this slot later. The market fields
    /// come from the LIVE config (`chart.market()`): the requested identity, so a switch
    /// still loading when the snapshot is taken (persist on close) is not lost.
    pub fn dehydrate(&self) -> PooledCellState {
        let chart = self.try_chart();
        let live = chart.as_ref().map(|c| c.market());
        let ext = if chart.is_some() {
            self.dehydrate_ext()
        } else {
            let d = self.inner.data.borrow();
            (!d.ext_state.is_empty()).then(|| d.ext_state.clone())
        };
        let prefs = self.status_prefs();
        let (state, fallback, pending, settled, volume_pending) = {
            let d = self.inner.data.borrow();
            (
                d.state.clone(),
                d.fallback.clone(),
                d.pending_manifest.clone(),
                self.observer().is_none_or(|o| o.manifest_settled()),
                (d.volume_may_be_pending && d.volume_intent)
                    || chart.as_ref().is_some_and(|c| {
                        c.pending_native_indicators().iter().any(|t| t == "volume")
                    }),
            )
        };
        // Identity from the live config; depth (`bars`) stays the cell's own durable
        // budget: in range mode the config carries the chip's transient fetch budget.
        let (symbol, provider, timeframe) = match &live {
            Some(m) => {
                let tf = match (&fallback, &m.symbol, &m.timeframe) {
                    (Some((fs, ft)), Some(ms), Some(mt)) if fs == ms && ft == mt => {
                        state.timeframe.clone()
                    }
                    _ => m.timeframe.clone(),
                };
                (m.symbol.clone(), m.provider.clone(), tf)
            }
            None => (
                state.symbol.clone(),
                state.provider.clone(),
                state.timeframe.clone(),
            ),
        };
        let present: Vec<PersistedNativeEntry> = if chart.is_some() {
            // Live handles carry what the registry read cannot: the input DELTAS and the
            // hidden flag, so native settings and visibility survive the round-trip.
            self.native_handles()
                .iter()
                .filter_map(|h| native_ledger_entry(h.as_ref()))
                .collect()
        } else {
            Vec::new()
        };
        let instance_entries: Vec<PersistedManifestEntry> = self
            .inner
            .data
            .borrow()
            .instances
            .iter()
            .filter(|it| !it.external)
            .map(|it| {
                // LIVE deltas from the handle; a handle-less instance (add failed) keeps
                // whatever values it was restored with.
                let (d, hidden) = match it.handle.borrow().as_ref() {
                    Some(h) => (instance_deltas(h.as_ref()), !h.visible()),
                    None => (it.values.borrow().clone(), false),
                };
                if d.is_some() || hidden {
                    let d = d.unwrap_or_default();
                    PersistedManifestEntry::Entry {
                        name: it.entry.name.clone(),
                        inputs: d.inputs.as_ref().map(inputs_to_map),
                        props: d.props.as_ref().map(inputs_to_map),
                        hidden: hidden.then_some(true),
                    }
                } else {
                    PersistedManifestEntry::Name(it.entry.name.clone())
                }
            })
            .collect();
        CellState {
            symbol,
            provider,
            timeframe,
            // The CHOSEN style while the symbol's data shows a fallback: a reload draws it
            // again.
            price_style: Some(self.requested_price_style()),
            bars: state.bars,
            session: state.session.map(|s| s.as_str().to_string()),
            watermark: Some(prefs.watermark),
            replay_watermark: Some(prefs.replay_watermark),
            indicator_titles: Some(prefs.indicator_titles),
            indicator_values: Some(prefs.indicator_values),
            statusline_parts: prefs.parts.map(|p| p.persisted()),
            renderer_config: chart.as_ref().and_then(|c| c.renderer().get_config()),
            drawings: chart
                .as_ref()
                .and_then(|c| serde_json::to_value(c.drawings().to_json()).ok()),
            indicators: Some(indicator_ledger(LedgerInputs {
                present,
                instance_entries,
                pending_manifest: pending,
                manifest_settled: settled,
                volume_pending,
            })),
            ext,
        }
    }

    /// Tear the cell down: the chart, its subscriptions and the undo timeline. The workspace
    /// then removes the container through `WorkspaceHost::destroy_cell`.
    pub fn destroy(&self) {
        if self.inner.destroyed.replace(true) {
            return;
        }
        let chart = self.inner.chart.borrow_mut().take();
        if let Some(chart) = &chart {
            for sub in self.inner.subs.borrow_mut().drain(..) {
                chart.off(&sub);
            }
        }
        for off in self.inner.offs.borrow_mut().drain(..) {
            off();
        }
        self.inner.history.destroy();
        if let Some(chart) = chart {
            chart.destroy();
        }
        *self.inner.renderer.borrow_mut() = None;
        let mut d = self.inner.data.borrow_mut();
        // The ledger the cell dies with: `present_natives` stays for `indicator_count`.
        d.instances.clear();
    }
}

/// How [`ChartCell::add_manifest_instance`] adds one instance.
#[derive(Clone, Debug)]
pub struct ManifestAddOptions {
    /// Record an undo entry (restores and the manifest seed do not).
    pub record: bool,
    /// Through the public seam: stays out of the persisted ledger.
    pub external: bool,
    pub id: Option<String>,
    pub inputs: Option<Inputs>,
    pub props: Option<Inputs>,
    pub hidden: bool,
}

impl Default for ManifestAddOptions {
    fn default() -> Self {
        Self {
            record: true,
            external: false,
            id: None,
            inputs: None,
            props: None,
            hidden: false,
        }
    }
}

impl ManifestAddOptions {
    /// A restored ledger entry: recorded values, never an undo entry.
    fn from_ledger(entry: &PersistedManifestEntry) -> Self {
        match entry {
            PersistedManifestEntry::Name(_) => Self {
                record: false,
                ..Self::default()
            },
            PersistedManifestEntry::Entry {
                inputs,
                props,
                hidden,
                ..
            } => Self {
                record: false,
                inputs: inputs.as_ref().map(map_to_inputs),
                props: props.as_ref().map(map_to_inputs),
                hidden: hidden.unwrap_or(false),
                ..Self::default()
            },
        }
    }
}

/// The names the indicator picker's search matches for a native type besides its shown
/// title: the type's own id (the original adds the English title and the short title in
/// both the chart's language and English from the native registry, which the widget's
/// picker owns; the type id is the one alias this module can state exactly).
fn native_aliases(indicator_type: &str) -> Vec<String> {
    vec![indicator_type.to_string()]
}

#[cfg(test)]
mod tests;
