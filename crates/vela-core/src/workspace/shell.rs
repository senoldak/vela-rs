//! `VelaWorkspace`: the multi-chart shell's logic (`VelaWorkspace.ts`): a grid of
//! [`ChartCell`]s behind ONE shared data feed, an active cell, sync links between cells,
//! resizable tracks, the whole-document state surface and persistence, theme and locale
//! propagation, and the workspace replay.
//!
//! # Shape
//!
//! [`Workspace`] is a cheap shared handle over one state struct behind a `RefCell`. An
//! operation takes short borrows and never holds one across a call into a cell, the chart,
//! the host or an event handler; the events a change owes subscribers are emitted after
//! the borrow is released, so a subscriber may call straight back in. The host is never
//! re-entered from inside one of its own methods' callbacks (see [`WorkspaceHost`]).
//!
//! The shared shell chrome (topbar, bottombar, mobile bar and drawers, dock, pickers,
//! toolbar, alerts menu, keymap) is the browser crate's. The original's projection rule
//! (the chrome holds no state: it rebinds from the ACTIVE cell on activation and on that
//! cell's own events) is kept as data and a signal: [`Workspace::projection`] reads the
//! active cell, and [`WorkspaceEvent::Projection`] fires on every trigger of the rule (an
//! activation, and any market, price-style, indicator, session or history change of the
//! active cell). Re-read the projection on it.
//!
//! # Not ported (browser crate)
//!
//! `Topbar`, `Bottombar`, `MobileBar` and the drawers (timeframe, drawings, more, timezone,
//! price scale), `ObjectTree`, `DataWindow`, `PanelDock`, `Sidebar`, `SymbolPicker`,
//! `IndicatorPicker`, `TimeframeQuick`, `ShortcutsHelp`, the toast surface, the alerts
//! dropdown, `DrawingToolbar` and `DrawingPill`, the keymap and its default chords
//! (`registerDefaultKeys`, `routeTyping`), the glider, the layout-mode controller, the
//! attribution mark, the CSS, regions and docked strips, contribution mounting
//! (`mountAttachments`, `refreshActions`, topbar item visibility) and the splitter DOM.
//! Their logic that is data is here: [`resolve_typing`], [`next_auto_cell_id`],
//! [`declared_order`], the sync rows ([`Workspace::sync_rows`]), the alerts list
//! ([`Workspace::alerts`]), the shared drawing-tool prefs and favorites, the screenshot
//! plan and the splitter geometry.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

use serde_json::Value;

use crate::data::multi_provider_feed::MultiProviderFeed;
use crate::drawings::SerializedDrawing;
use crate::drawings::controller::{AddInit, DrawingUpdate};
use crate::events::types::{ChartEvent, ChartEventName};
use crate::i18n::{
    LocaleChangedEvent, LocaleState, LocalizationOptions, LocalizationPatch, MessageArg,
    MessageLike, Translator, msg, text_of,
};
use crate::options::{NativeBackend, ThemeSpec, VelaOptions};
use crate::ports::chart_renderer::{LayoutMode, WallClock};
use crate::ports::data_provider::DataProvider;
use crate::runtime::{Platform, TimerId};
use crate::script_run::ScriptRun;
use crate::state::{
    ChartState, DrawingToolsState, Magnet, PanelsState, STATE_VERSION, SyncKind, SyncOptions,
    SyncSetting, TrackSizes, WorkspaceState, decode_state, encode_state, sanitize_state,
};
use crate::vela::{Vela, VelaEnv};

use super::cell::{
    CellBoot, CellDeps, CellExtHandler, CellObserver, CellSeed, ChartCell, EngineFactories,
    ManifestIndicator, PooledCellState, cell_chart_defaults, seed_defaults,
};
use super::host::{CellPlacement, GridPresentation, ToastKind, WorkspaceHost};
use super::layouts::{
    LayoutDefinition, LayoutRegistry, active_after_layout, grid_styles, occupancy_grid,
    order_after_layout,
};
use super::persist::{StorageRead, WorkspaceStorage};
use super::replay::{ReplayCell, WorkspaceReplay, WorkspaceReplayHost};
use super::screenshot::{
    LayoutShotFrame, LayoutShotTile, ShotRect, composite_layout_screenshot_with, dest_rect,
    tiles_from_cell_rects,
};
use super::splitters::{Axis, SplitterStrip, even_tracks, splitter_strips};
use super::sync::{
    CellDrawings, drawing_link_content, pair_identical_drawings, ranges_within, style_config_patch,
    sync_groups, sync_targets,
};
use crate::widget::timeframe::timeframe_label;

const GAP_PX: f64 = 2.0; // grid gap: the visible seam between cells (splitter strips center on it)
const POOL_CAP: usize = 16; // dormant slot states kept across layout shrinks
const ALERT_CAP: usize = 50;
/// The debounce of the `state:changed` notification and the persistence write.
const STATE_DEBOUNCE_MS: f64 = 500.0;
/// The persistence key `persist: true` stands for.
pub const DEFAULT_PERSIST_KEY: &str = "vela-workspace";

/// The workspace's own messages.
struct Text;

impl Text {
    fn alert() -> MessageLike {
        msg("workspace.alerts.untitled", "Alert")
    }

    fn sync_symbol() -> MessageLike {
        msg("workspace.sync.symbol", "Symbol")
    }

    fn sync_interval() -> MessageLike {
        msg("workspace.sync.interval", "Interval")
    }

    fn sync_crosshair() -> MessageLike {
        msg("workspace.sync.crosshair", "Crosshair")
    }

    fn sync_style() -> MessageLike {
        msg("workspace.sync.style", "Style")
    }
}

// ── pure helpers ───────────────────────────────────────────────

/// The cell identities the `cells` option declares, in DECLARATION order: a cell's NAME
/// never encodes its position (the first declared entry fills the first layout slot, and
/// so on). Purely-numeric names are dropped with a warning: JS object enumeration would
/// reorder integer-like keys ahead of everything, silently breaking the declared order, so
/// the original rejects them and this port keeps the rule for documents written for it.
pub fn declared_order(
    cells: &[(String, CellSeed)],
    platform: Option<&dyn Platform>,
) -> Vec<String> {
    let numeric = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    for (n, _) in cells {
        if numeric(n)
            && let Some(p) = platform
        {
            p.warn(&format!(
                "[vela] workspace cell \"{n}\" ignored \u{2014} a purely-numeric name cannot keep its declaration order (JS object key semantics); use e.g. \"cell{n}\""
            ));
        }
    }
    cells
        .iter()
        .filter(|(n, _)| !numeric(n))
        .map(|(n, _)| n.clone())
        .collect()
}

/// The first `c<N>` name not already taken: identities for slots beyond the declared list.
pub fn next_auto_cell_id(taken: &std::collections::HashSet<String>) -> String {
    let mut i = 1;
    loop {
        let id = format!("c{i}");
        if !taken.contains(&id) {
            return id;
        }
        i += 1;
    }
}

/// Which dialogs are up when a bare keystroke reaches the workspace root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypingState {
    pub dialogs: usize,
    pub symbol_search: bool,
    pub timeframe_entry: bool,
}

/// Where a bare keystroke goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypingTarget {
    Symbol,
    Timeframe,
}

/// What a bare keystroke on the workspace root does. Nothing open: a letter or `0` seeds
/// symbol search (no timeframe starts with 0), a digit 1-9 the timeframe entry. An ENTRY
/// dialog open: its field takes focus a tick after the dialog reports open, so the next
/// key of a fast typist (`NQ`, `15`) still lands on the root: it is handed to that dialog
/// as more typed text instead of dropped. Any other open dialog swallows bare typing.
pub fn resolve_typing(key: &str, open: TypingState) -> Option<(TypingTarget, String)> {
    // One printable character (a code point: a letter outside the Basic Multilingual Plane
    // counts once), not a control key name like `Enter`.
    let mut chars = key.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    };
    if open.dialogs > 0 {
        single?;
        if open.symbol_search {
            return Some((TypingTarget::Symbol, key.to_uppercase()));
        }
        if open.timeframe_entry {
            return Some((TypingTarget::Timeframe, key.to_uppercase()));
        }
        return None;
    }
    // A letter of any script opens symbol search: tickers are searched by name too.
    if let Some(c) = single {
        if c.is_alphabetic() || c == '0' {
            return Some((TypingTarget::Symbol, key.to_uppercase()));
        }
        if ('1'..='9').contains(&c) {
            return Some((TypingTarget::Timeframe, key.to_string()));
        }
    }
    None
}

// ── options ────────────────────────────────────────────────────

/// How the grid's initial layout is given.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutChoice {
    /// A registered id (`"1"`, `"2h"`, `"2v"`, `"4"`, `"8"`, a picker id `g3x2`, or a
    /// plugin's).
    Id(String),
    /// An inline definition.
    Definition(LayoutDefinition),
    /// `layout: false`: SINGLE-CHART mode. The grid is pinned to the one-cell layout, the
    /// layout picker disappears (the sync switches with it), `set_layout` becomes a no-op,
    /// and a restored document keeps only its first chart live (the rest stays dormant in
    /// the pool).
    Single,
}

/// Third-party document-level state (`registerStatePersistence` with scope `global`).
pub trait GlobalExtHandler {
    /// The namespaced key (`vendor.feature`).
    fn key(&self) -> &str;
    /// The value to persist, or `None` to withdraw the entry.
    fn serialize(&self, ws: &Workspace) -> Option<Value>;
    /// Re-apply a persisted value (keys present in the document only).
    fn restore(&self, value: &Value, ws: &Workspace);
}

/// The workspace options: the widget's chart vocabulary (as `chart`) plus the grid's own.
/// Every chart option given in `chart` (symbol, timeframe, price style, colors, glow,
/// default language, ...) is the DEFAULT of each cell; `cells` overrides it per cell with
/// the same words. `height` is the one chart option a grid cannot honor (the layout sizes
/// cells).
pub struct WorkspaceOptions {
    pub chart: VelaOptions,
    /// Initial layout. Default `"4"`.
    pub layout: LayoutChoice,
    /// Per-cell overrides keyed by a FREE-FORM cell name: the name is the cell's durable
    /// IDENTITY (persistence, sync groups, `ws.cell(name)`), never its position:
    /// DECLARATION ORDER fills the layout's slots (first declared, first slot). Fewer
    /// entries than slots: the remaining slots boot on the defaults (auto identity); more:
    /// the extras wait in the pool and appear when a larger layout reveals them.
    pub cells: Vec<(String, CellSeed)>,
    /// Sync links between cells (default: everything off).
    pub sync: SyncOptions,
    /// Above this many cells, EVERY cell uses the canvas2d backend. Default 8; an explicit
    /// backend other than `Auto` wins.
    pub max_webgl_cells: usize,
    /// How many alerts the bell keeps (the oldest drop beyond it). Default 50.
    pub alert_cap: usize,
    /// The initial display timezone choice (an IANA zone or `exchange`). Default `Etc/UTC`.
    pub timezone: Option<String>,
    /// Persist the state document under this key (`DEFAULT_PERSIST_KEY` for `persist: true`).
    pub persist: Option<String>,
    /// Where the document is persisted. Without one, `persist` has nowhere to write.
    pub storage: Option<Rc<dyn WorkspaceStorage>>,
    /// Data providers, registered once on the shared feed, in declaration order.
    pub providers: Vec<(String, Rc<dyn DataProvider>)>,
    /// Scripting engines, instantiated per cell.
    pub engines: EngineFactories,
    /// The resolved indicator manifest (the original resolves an `indicators` option
    /// asynchronously; resolve it and pass it here, or call
    /// [`Workspace::set_manifest`] when it lands). `None`: no manifest will ever resolve.
    pub indicators: Option<Vec<ManifestIndicator>>,
    /// An `indicators` option exists but has not resolved yet: until
    /// [`Workspace::set_manifest`] runs, a restored cell keeps reporting the manifest entries
    /// it was restored with (an early save must not wipe them).
    pub manifest_pending: bool,
    /// Status lines on every cell (default true).
    pub statusline: bool,
    /// Symbol watermark on every cell (default true).
    pub watermark: bool,
    /// The layouts the workspace resolves ids against. Default: the built-ins.
    pub layouts: LayoutRegistry,
    pub cell_ext_handlers: Vec<Rc<dyn CellExtHandler>>,
    pub global_ext_handlers: Vec<Rc<dyn GlobalExtHandler>>,
}

impl Default for WorkspaceOptions {
    fn default() -> Self {
        Self {
            chart: VelaOptions::default(),
            layout: LayoutChoice::Id("4".into()),
            cells: Vec::new(),
            sync: SyncOptions::default(),
            max_webgl_cells: 8,
            alert_cap: ALERT_CAP,
            timezone: None,
            persist: None,
            storage: None,
            providers: Vec::new(),
            engines: Vec::new(),
            indicators: None,
            manifest_pending: false,
            statusline: true,
            watermark: true,
            layouts: LayoutRegistry::with_builtins(),
            cell_ext_handlers: Vec::new(),
            global_ext_handlers: Vec::new(),
        }
    }
}

/// Why building or driving a workspace failed.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceError {
    /// A layout id that is neither registered nor a picker grid id.
    UnknownLayout(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownLayout(id) => write!(
                f,
                "[vela] unknown workspace layout \"{id}\" \u{2014} register it with registerLayout()."
            ),
        }
    }
}

impl std::error::Error for WorkspaceError {}

// ── events ─────────────────────────────────────────────────────

/// A script run, tagged with the cell it ran in.
#[derive(Clone)]
pub struct WorkspaceScriptRun {
    pub run: ScriptRun,
    pub cell: String,
}

/// The workspace's events.
#[derive(Clone)]
pub enum WorkspaceEvent {
    /// The active cell changed (click or focus in a cell, or `set_active_cell`).
    CellActive {
        id: String,
        prev: Option<String>,
    },
    /// A script computed in ANY cell: the per-chart `script:run` relayed up with its cell
    /// identity, so one subscription covers the whole grid.
    ScriptRun(Box<WorkspaceScriptRun>),
    /// One cell's price style is switching `from` to `to`: the per-chart
    /// `priceStyle:change` relayed up with the cell's identity. Same timing as the chart
    /// event: synchronous, before that cell repaints.
    CellPriceStyle {
        id: String,
        from: String,
        to: String,
    },
    /// The grid switched layouts (cells created, destroyed or restored around it).
    LayoutChanged {
        layout: String,
    },
    /// A cell was maximized over the whole grid, or the grid restored (`None`).
    CellMaximized {
        id: Option<String>,
    },
    CellCreated {
        id: String,
    },
    CellDestroyed {
        id: String,
    },
    /// The persistable state changed (debounced about 500 ms): re-pull `get_state()` if you
    /// consume it. The signal custom persistence flows build on.
    StateChanged,
    /// The workspace's localization changed, after every cell re-rendered.
    LocaleChanged(LocaleChangedEvent),
    /// The shared chrome should re-read [`Workspace::projection`]: the active cell
    /// changed, or the active cell's own market, price style, indicators, session or
    /// history did (the projection rule's two triggers).
    Projection {
        id: String,
    },
    /// A sync link changed (the layout dropdown's switches re-read [`Workspace::sync_rows`]).
    SyncChanged,
    /// The display timezone choice changed.
    TimezoneChanged,
    /// The alerts list changed.
    AlertsChanged,
    /// The shared favorites (drawing tools, timeframes) changed.
    FavoritesChanged,
    /// The shared drawing-tool prefs (remembered settings, templates, magnet, stay,
    /// armed tool) changed.
    DrawingToolsChanged,
    /// One cell's status-line / watermark prefs changed (a segment toggle, titles, values,
    /// the watermark), on ANY cell, active or not, and on a follower the style link
    /// mirrored them onto. The overlays re-read [`ChartCell::status_prefs`] of `id`.
    StatusPrefs {
        id: String,
    },
    /// [`Workspace::apply_state`] / [`Workspace::apply_state_doc`] restored the dock's
    /// `panels` slice (an absent slice closes the column): re-read
    /// [`Workspace::panels_state`]. Raised after the whole document was applied.
    PanelsRestored,
}

/// The names of [`WorkspaceEvent`]s, for [`Workspace::on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkspaceEventName {
    CellActive,
    ScriptRun,
    CellPriceStyle,
    LayoutChanged,
    CellMaximized,
    CellCreated,
    CellDestroyed,
    StateChanged,
    LocaleChanged,
    Projection,
    SyncChanged,
    TimezoneChanged,
    AlertsChanged,
    FavoritesChanged,
    DrawingToolsChanged,
    StatusPrefs,
    PanelsRestored,
}

impl WorkspaceEvent {
    pub fn name(&self) -> WorkspaceEventName {
        use WorkspaceEventName as N;
        match self {
            Self::CellActive { .. } => N::CellActive,
            Self::ScriptRun(_) => N::ScriptRun,
            Self::CellPriceStyle { .. } => N::CellPriceStyle,
            Self::LayoutChanged { .. } => N::LayoutChanged,
            Self::CellMaximized { .. } => N::CellMaximized,
            Self::CellCreated { .. } => N::CellCreated,
            Self::CellDestroyed { .. } => N::CellDestroyed,
            Self::StateChanged => N::StateChanged,
            Self::LocaleChanged(_) => N::LocaleChanged,
            Self::Projection { .. } => N::Projection,
            Self::SyncChanged => N::SyncChanged,
            Self::TimezoneChanged => N::TimezoneChanged,
            Self::AlertsChanged => N::AlertsChanged,
            Self::FavoritesChanged => N::FavoritesChanged,
            Self::DrawingToolsChanged => N::DrawingToolsChanged,
            Self::StatusPrefs { .. } => N::StatusPrefs,
            Self::PanelsRestored => N::PanelsRestored,
        }
    }
}

/// A handle to one registered event handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceSubscription(u64);

/// One alert the bell keeps, tagged by the cell it fired in.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceAlert {
    pub cell_id: String,
    /// Provenance the way a user reads it: symbol, timeframe and the indicator that fired,
    /// captured at fire time.
    pub source: String,
    pub title: String,
    pub message: String,
    pub time: i64,
}

/// What the shared chrome reads from the ACTIVE cell.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveProjection {
    pub cell_id: String,
    pub symbol: String,
    pub timeframe: String,
    pub price_style: String,
    pub unavailable_price_styles: Vec<String>,
    pub indicator_count: usize,
    pub can_undo: bool,
    pub can_redo: bool,
    pub active_range_id: Option<String>,
    pub session: crate::ports::market_data::MarketSession,
    pub session_available: bool,
    pub timezone_choice: String,
    pub exchange_timezone: Option<String>,
    pub drawing_tool: Option<String>,
    pub magnet: Magnet,
    pub stay: bool,
    pub favorites: Vec<String>,
    pub maximized: Option<String>,
}

/// One row of the layout dropdown's sync switches.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncRow {
    pub id: &'static str,
    pub label: String,
    pub checked: bool,
}

// ── state ──────────────────────────────────────────────────────

struct State {
    def: LayoutDefinition,
    /// Cell identities by SLOT POSITION: `order[i]` lives in the layout's i-th slot. Names
    /// come from the `cells` declaration order (then the persisted document); slots beyond
    /// the list get auto identities. Grows, never reorders (except a layout change that
    /// protects the active cell).
    order: Vec<String>,
    cells: HashMap<String, ChartCell>,
    pool: Vec<(String, PooledCellState)>,
    track_sizes: BTreeMap<String, TrackSizes>,
    active_id: Option<String>,
    /// The cell maximized over the whole grid: TRANSIENT view state, never persisted.
    maximized_id: Option<String>,
    drop_target: Option<String>,
    /// The shell's chrome size class, pushed into every cell's renderer (and into cells born later).
    layout_mode: LayoutMode,
    /// The host's one second pulse: every cell's countdown chip ticks on it, never on a
    /// renderer-private timer that could read another second.
    wall_clock: Option<Rc<dyn WallClock>>,
    cell_backend: NativeBackend,
    theme: ThemeSpec,
    timezone: String,
    sync: SyncOptions,
    favs: Vec<String>,
    tf_favs: Vec<String>,
    tool_defs: BTreeMap<String, Value>,
    tool_templates: BTreeMap<String, Vec<Value>>,
    /// The GLOBAL armed tool, magnet and stay (workspace policy): re-applied to whichever
    /// cell takes the focus; only the ACTIVE cell ever holds a non-null tool.
    global_tool: Option<String>,
    global_snap: crate::drawings::SnapMode,
    global_stay: bool,
    panels: Option<PanelsState>,
    manifest: Vec<ManifestIndicator>,
    manifest_settled: bool,
    alerts: Vec<WorkspaceAlert>,
    ext_state: BTreeMap<String, Value>,
    /// LINKED drawings (the drawings sync): one group per synced set (cell id to that
    /// cell's drawing id), reachable from every member under its `cell\0drawing` key. Never
    /// persisted: rebuilt from content.
    drawing_links: HashMap<String, Rc<RefCell<BTreeMap<String, String>>>>,
}

type Handler = Rc<dyn Fn(&WorkspaceEvent)>;
type HandlerEntry = (u64, WorkspaceEventName, Handler);
type ChartHandler = Box<dyn Fn(&Workspace, &ChartEvent)>;

struct WsInner {
    host: Rc<dyn WorkspaceHost>,
    env: VelaEnv,
    platform: Rc<dyn Platform>,
    feed: MultiProviderFeed,
    layouts: LayoutRegistry,
    storage: Option<Rc<dyn WorkspaceStorage>>,
    persist_key: Option<String>,
    mono_layout: bool,
    alert_cap: usize,
    chart_opts: RefCell<VelaOptions>,
    seeds: Vec<(String, CellSeed)>,
    engines: EngineFactories,
    statusline: bool,
    watermark: bool,
    max_webgl_cells: usize,
    ext_cell: Vec<Rc<dyn CellExtHandler>>,
    ext_global: Vec<Rc<dyn GlobalExtHandler>>,
    s: RefCell<State>,
    handlers: RefCell<Vec<HandlerEntry>>,
    next_handler: Cell<u64>,
    replay: WorkspaceReplay,
    locale: RefCell<Rc<LocaleState>>,
    /// Re-entrance guard around one propagation tick: followers' synchronous echoes (their
    /// `set_visible_range` re-emits `viewport:changed`) must not re-propagate.
    sync_busy: Cell<bool>,
    /// Same guard for the drawings link.
    drawing_sync_busy: Cell<bool>,
    /// Same guard for the style link: a follower's `apply_config` re-fires its config
    /// notification in the same tick, and a state restore applies per-cell configs that
    /// legitimately differ.
    style_sync_busy: Cell<bool>,
    /// Re-entrancy guard while one cell's remembered settings fan out to the others.
    tool_defs_busy: Cell<bool>,
    destroyed: Cell<bool>,
    /// False until the constructor finishes. Boot is not an edit: seeding the sync settings
    /// marks state dirty, so without this a workspace mounted and torn down inside the
    /// debounce window would flush and tell the host the user had changed something.
    booted: Cell<bool>,
    /// Re-entrance guard of [`flush_pending_state`](Workspace::flush_pending_state).
    emitting: Cell<bool>,
    state_timer: Cell<Option<TimerId>>,
    me: Weak<WsInner>,
}

/// The multi-chart workspace. A cheap shared handle; see the module documentation.
#[derive(Clone)]
pub struct Workspace {
    inner: Rc<WsInner>,
}

/// A non-owning handle to a [`Workspace`].
#[derive(Clone)]
pub struct WeakWorkspace {
    inner: Weak<WsInner>,
}

impl WeakWorkspace {
    pub fn upgrade(&self) -> Option<Workspace> {
        self.inner.upgrade().map(|inner| Workspace { inner })
    }

    /// A handle that never upgrades (for a host object that outlives its workspace and must
    /// still be built, e.g. a context handed out after the workspace was destroyed).
    pub fn dangling() -> Self {
        Self { inner: Weak::new() }
    }
}

struct ReplayHostImpl {
    ws: Weak<WsInner>,
}

impl WorkspaceReplayHost for ReplayHostImpl {
    fn cells(&self) -> Vec<ReplayCell> {
        let Some(inner) = self.ws.upgrade() else {
            return Vec::new();
        };
        Workspace { inner }
            .cells()
            .into_iter()
            .filter_map(|c| {
                Some(ReplayCell {
                    id: c.id().to_string(),
                    chart: c.try_chart()?,
                })
            })
            .collect()
    }

    fn active_id(&self) -> Option<String> {
        self.ws.upgrade()?.s.borrow().active_id.clone()
    }
}

fn pick_localization(opts: &LocalizationOptions) -> LocalizationOptions {
    opts.clone()
}

impl Workspace {
    /// Build the workspace on `host`: resolve the layout, boot the persisted document when
    /// the storage answers at once (no flash of defaults; an async store late-applies
    /// through [`apply_state`](Self::apply_state)), register the providers on the shared
    /// feed, and create the cells.
    pub fn new(
        host: Rc<dyn WorkspaceHost>,
        env: VelaEnv,
        opts: WorkspaceOptions,
    ) -> Result<Workspace, WorkspaceError> {
        let platform = env.platform.clone();
        let feed = MultiProviderFeed::new(env.bar_store.clone(), platform.clone());
        let locale = Rc::new(LocaleState::new(
            pick_localization(&opts.chart.localization),
            env.intl.clone(),
            env.locales.clone(),
        ));
        let persist_key = opts.persist.clone();
        let mut boot: Option<WorkspaceState> = None;
        let mut late: Option<crate::runtime::LocalBoxFuture<'static, Option<String>>> = None;
        if persist_key.is_some()
            && let Some(storage) = &opts.storage
            && let Some(key) = &persist_key
        {
            match storage.get(key) {
                StorageRead::Ready(Some(raw)) => boot = decode_state(&raw),
                StorageRead::Ready(None) => {}
                StorageRead::Pending(fut) => late = Some(fut),
            }
        }
        let mono_layout = matches!(opts.layout, LayoutChoice::Single);
        let opt_layout = match &opts.layout {
            LayoutChoice::Id(id) => Some(
                opts.layouts
                    .ensure(id)
                    .ok_or_else(|| WorkspaceError::UnknownLayout(id.clone()))?,
            ),
            LayoutChoice::Definition(def) => Some(def.clone()),
            LayoutChoice::Single => None,
        };
        let booted_def = boot.as_ref().and_then(|b| opts.layouts.ensure(&b.layout));
        // Single-chart mode pins the grid to '1': a persisted document's layout is
        // presentation the mode overrides, while its charts still restore.
        let def = if mono_layout {
            opts.layouts
                .ensure("1")
                .ok_or_else(|| WorkspaceError::UnknownLayout("1".into()))?
        } else {
            booted_def
                .or(opt_layout)
                .or_else(|| opts.layouts.ensure("4"))
                .ok_or_else(|| WorkspaceError::UnknownLayout("4".into()))?
        };
        let timezone = boot
            .as_ref()
            .and_then(|b| b.timezone.clone())
            .or_else(|| opts.timezone.clone())
            .unwrap_or_else(|| "Etc/UTC".into());
        let mut state = State {
            def,
            order: Vec::new(),
            cells: HashMap::new(),
            pool: Vec::new(),
            track_sizes: BTreeMap::new(),
            active_id: None,
            maximized_id: None,
            drop_target: None,
            layout_mode: LayoutMode::Desktop,
            wall_clock: None,
            cell_backend: NativeBackend::Auto,
            theme: opts
                .chart
                .theme
                .clone()
                .unwrap_or_else(|| ThemeSpec::from("dark")),
            timezone,
            sync: SyncOptions::default(),
            favs: Vec::new(),
            tf_favs: Vec::new(),
            tool_defs: BTreeMap::new(),
            tool_templates: BTreeMap::new(),
            global_tool: None,
            global_snap: crate::drawings::SnapMode::Off,
            global_stay: false,
            panels: None,
            manifest: Vec::new(),
            manifest_settled: opts.indicators.is_none() && !opts.manifest_pending,
            alerts: Vec::new(),
            ext_state: BTreeMap::new(),
            drawing_links: HashMap::new(),
        };
        let sync = boot
            .as_ref()
            .and_then(|b| b.sync.clone())
            .unwrap_or_else(|| opts.sync.clone());
        for kind in SyncKind::ALL {
            match sync.get(kind) {
                None | Some(SyncSetting::Flag(false)) => {}
                Some(setting) => state.sync.set(kind, Some(setting.clone())),
            }
        }
        if let Some(b) = &boot {
            if let Some(f) = &b.favorites {
                state.favs = f.clone();
            }
            if let Some(f) = &b.timeframe_favorites {
                state.tf_favs = f.clone();
            }
            if let Some(t) = &b.drawing_tools {
                Self::adopt_tools(&mut state, t);
            }
            if let Some(ts) = &b.track_sizes {
                state.track_sizes = ts.clone();
            }
            if let Some(ext) = &b.ext {
                state.ext_state = ext.clone();
            }
            state.panels = b.panels.clone();
            for chart in &b.charts {
                state.pool.push((chart.id.clone(), chart.cell.clone()));
            }
        }
        state.order = match &boot {
            Some(b) => b.charts.iter().map(|c| c.id.clone()).collect(),
            None => declared_order(&opts.cells, Some(platform.as_ref())),
        };
        let boot_active = boot.as_ref().and_then(|b| b.active_cell_id.clone());
        let WorkspaceOptions {
            chart,
            cells,
            max_webgl_cells,
            alert_cap,
            providers,
            engines,
            indicators,
            statusline,
            watermark,
            layouts,
            cell_ext_handlers,
            global_ext_handlers,
            storage,
            ..
        } = opts;
        if let Some(list) = indicators {
            state.manifest = list;
            state.manifest_settled = true;
        }
        let inner = Rc::new_cyclic(|me: &Weak<WsInner>| WsInner {
            host,
            env,
            platform: platform.clone(),
            feed,
            layouts,
            storage,
            persist_key,
            mono_layout,
            alert_cap: alert_cap.max(1),
            chart_opts: RefCell::new(chart),
            seeds: cells,
            engines,
            statusline,
            watermark,
            max_webgl_cells,
            ext_cell: cell_ext_handlers,
            ext_global: global_ext_handlers,
            s: RefCell::new(state),
            handlers: RefCell::default(),
            next_handler: Cell::new(0),
            // Before any cell exists: the replay watches every cell from its creation on.
            replay: WorkspaceReplay::new(
                Rc::new(ReplayHostImpl { ws: me.clone() }),
                platform.clone(),
            ),
            locale: RefCell::new(locale),
            sync_busy: Cell::new(false),
            drawing_sync_busy: Cell::new(false),
            style_sync_busy: Cell::new(false),
            tool_defs_busy: Cell::new(false),
            destroyed: Cell::new(false),
            booted: Cell::new(false),
            emitting: Cell::new(false),
            state_timer: Cell::new(None),
            me: me.clone(),
        });
        let ws = Workspace { inner };
        // ONE shared feed: providers registered once; every cell's `chart.data()` operates
        // on the same registry, symbol index and closed-bar cache.
        for (name, provider) in providers {
            // The registration starts at once; the index settles through `feed.ready()`.
            drop(ws.inner.feed.register_provider(&name, provider));
        }
        {
            let weak = ws.downgrade();
            let ready = ws.inner.feed.ready();
            platform.spawn(Box::pin(async move {
                ready.await;
                if let Some(ws) = weak.upgrade()
                    && !ws.inner.destroyed.get()
                {
                    ws.refresh_retention(); // re-key on canonical tickers once indexes settle
                }
            }));
        }
        if let Some(fut) = late {
            let weak = ws.downgrade();
            platform.spawn(Box::pin(async move {
                let raw = fut.await;
                if let Some(ws) = weak.upgrade()
                    && !ws.inner.destroyed.get()
                    && let Some(raw) = raw
                    && let Some(doc) = decode_state(&raw)
                {
                    ws.apply_state_doc(doc);
                }
            }));
        }
        {
            let backend = ws.backend_for(&ws.inner.s.borrow().def);
            ws.inner.s.borrow_mut().cell_backend = backend;
        }
        ws.apply_grid();
        ws.build_cells();
        ws.relink_identical_drawings(); // a persisted document's copies link again
        let first = ws.inner.s.borrow().order.first().cloned();
        let active = match boot_active {
            Some(a) if ws.inner.s.borrow().cells.contains_key(&a) => Some(a),
            _ => first,
        };
        ws.set_active_cell(active.as_deref());
        ws.restore_global_ext();
        // Built. From here a dirty mark means the USER changed something, and a teardown is
        // allowed to flush it; everything above was setup.
        ws.inner.booted.set(true);
        Ok(ws)
    }

    pub fn downgrade(&self) -> WeakWorkspace {
        WeakWorkspace {
            inner: Rc::downgrade(&self.inner),
        }
    }

    // ── access ──

    /// The cell with identity `id` (its declared name, or `c<N>` when undeclared), or
    /// `None` when no live cell holds it.
    pub fn cell(&self, id: &str) -> Option<ChartCell> {
        self.inner.s.borrow().cells.get(id).cloned()
    }

    /// Every live cell, in slot order. Enumerated over `order` (the IDENTITY space), never
    /// the layout's positional slot ids. Identities past the current layout size are
    /// pooled, not live, so they drop out here.
    pub fn cells(&self) -> Vec<ChartCell> {
        let s = self.inner.s.borrow();
        s.order
            .iter()
            .filter_map(|id| s.cells.get(id).cloned())
            .collect()
    }

    /// The ACTIVE cell: the one the shared chrome reflects and acts on.
    pub fn active(&self) -> Option<ChartCell> {
        let s = self.inner.s.borrow();
        s.active_id.as_ref().and_then(|id| s.cells.get(id).cloned())
    }

    pub fn active_id(&self) -> Option<String> {
        self.inner.s.borrow().active_id.clone()
    }

    /// Shortcut for `active().chart()`. LIVE: read it at the point of use; the durable
    /// identity to hold is the cell (or its id).
    pub fn chart(&self) -> Option<Vela> {
        self.active()?.try_chart()
    }

    /// The shared market-data registry feed (providers are registered on it once).
    pub fn feed(&self) -> &MultiProviderFeed {
        &self.inner.feed
    }

    /// The layouts this workspace resolves ids against.
    pub fn layouts(&self) -> &LayoutRegistry {
        &self.inner.layouts
    }

    /// The workspace's bar replay across every cell.
    pub fn replay(&self) -> WorkspaceReplay {
        self.inner.replay.clone()
    }

    pub fn is_destroyed(&self) -> bool {
        self.inner.destroyed.get()
    }

    /// Make cell `id` the active one (a pointer-down or focus in it, or a host call). A
    /// no-op when it already is, or after destroy.
    pub fn set_active_cell(&self, id: Option<&str>) {
        if self.inner.destroyed.get() {
            return;
        }
        let (prev, prevent) = {
            let s = self.inner.s.borrow();
            (s.active_id.clone(), s.active_id.as_deref() == id)
        };
        if prevent {
            return;
        }
        let prev_cell = {
            let mut s = self.inner.s.borrow_mut();
            // Switch FIRST: the departing cell's drawing:* events must not re-enter the mirrors.
            s.active_id = id.map(str::to_string);
            prev.as_ref().and_then(|p| s.cells.get(p).cloned())
        };
        if let Some(cell) = prev_cell
            && let Some(chart) = cell.try_chart()
        {
            // Only the ACTIVE cell ever holds an armed tool or a transient mode: the global
            // tool re-arms on the next activation; measure/eraser don't follow.
            chart.drawings().set_tool(None);
            chart.drawings().set_mode(None);
        }
        self.present_grid();
        if let Some(id) = id {
            self.project_active_cell(); // trigger 1: the chrome rebinds to the new active cell
            self.emit(&WorkspaceEvent::CellActive {
                id: id.to_string(),
                prev,
            });
            self.inner.replay.cell_active(id);
            self.mark_state_dirty();
        }
    }

    /// A pointer-down or focus reached cell `id`'s container: the host wires its listeners
    /// to this.
    pub fn activate(&self, id: &str) {
        if self.inner.s.borrow().cells.contains_key(id) {
            self.set_active_cell(Some(id));
        }
    }

    // ── events ──

    /// Listen to one event. Handlers run in registration order, after the operation's
    /// borrows are released: one may call back into the workspace.
    pub fn on(
        &self,
        name: WorkspaceEventName,
        handler: impl Fn(&WorkspaceEvent) + 'static,
    ) -> WorkspaceSubscription {
        let id = self.inner.next_handler.get();
        self.inner.next_handler.set(id + 1);
        self.inner
            .handlers
            .borrow_mut()
            .push((id, name, Rc::new(handler)));
        WorkspaceSubscription(id)
    }

    pub fn off(&self, sub: WorkspaceSubscription) -> bool {
        let mut handlers = self.inner.handlers.borrow_mut();
        let before = handlers.len();
        handlers.retain(|(id, _, _)| *id != sub.0);
        handlers.len() != before
    }

    fn emit(&self, event: &WorkspaceEvent) {
        let name = event.name();
        let handlers: Vec<Handler> = self
            .inner
            .handlers
            .borrow()
            .iter()
            .filter(|(_, n, _)| *n == name)
            .map(|(_, _, h)| h.clone())
            .collect();
        for h in handlers {
            h(event);
        }
    }

    // ── locale, theme, timezone ──

    /// The workspace's translator: its resolved locale and direction, `t()` for messages
    /// and `format()` for numbers and dates. After [`set_locale`](Self::set_locale) this
    /// returns the NEW translator (the old object keeps its old settings).
    pub fn i18n(&self) -> Rc<dyn Translator> {
        self.inner.locale.borrow().clone() as Rc<dyn Translator>
    }

    /// Change the language of the whole workspace at runtime: a locale tag.
    pub fn set_locale(&self, tag: &str) {
        self.update_localization(LocalizationPatch::locale(tag));
    }

    /// Change any localization fields at runtime. Fields set in `patch` replace their
    /// current value; the others are kept. Every cell re-renders in place and
    /// `locale:changed` fires.
    pub fn update_localization(&self, patch: LocalizationPatch) {
        if self.inner.destroyed.get() {
            return;
        }
        let next = {
            let old = self.inner.locale.borrow();
            let mut next = LocaleState::new(
                old.options().clone(),
                old.intl().clone(),
                old.registry().clone(),
            );
            next.update(patch.clone());
            Rc::new(next)
        };
        *self.inner.locale.borrow_mut() = next.clone();
        for cell in self.cells() {
            if let Some(chart) = cell.try_chart() {
                chart.update_localization(patch.clone());
            }
            cell.relocalize();
        }
        self.emit(&WorkspaceEvent::LocaleChanged(LocaleChangedEvent {
            locale: next.locale().to_string(),
            direction: next.direction(),
        }));
    }

    fn t(&self, message: &MessageLike, values: &[(&str, MessageArg)]) -> String {
        let locale = self.i18n();
        text_of(Some(locale.as_ref()), message, values)
    }

    /// The workspace-global display timezone CHOICE (an IANA zone, or `exchange`).
    pub fn timezone(&self) -> String {
        self.inner.s.borrow().timezone.clone()
    }

    /// Set the workspace-global display timezone, applied to EVERY cell. An IANA zone, or
    /// `"exchange"` (the exchange rule): each cell then renders in its OWN market's zone,
    /// and the bottom bar follows the active cell's.
    pub fn set_timezone(&self, zone: &str) {
        self.inner.s.borrow_mut().timezone = zone.to_string();
        self.emit(&WorkspaceEvent::TimezoneChanged);
        for cell in self.cells() {
            cell.apply_timezone();
        }
        self.mark_state_dirty();
    }

    /// Swap the workspace theme at runtime: a built-in name, a registered id or a full
    /// custom theme, applied to EVERY cell. Also reached from any cell's chart settings
    /// (Canvas, Theme). The choice sticks: cells rebuilt by later layout switches
    /// reconstruct with it.
    pub fn set_theme(&self, theme: impl Into<ThemeSpec>) {
        if self.inner.destroyed.get() {
            return;
        }
        let theme = theme.into();
        self.inner.s.borrow_mut().theme = theme.clone();
        self.inner.chart_opts.borrow_mut().theme = Some(theme.clone());
        // Each cell's own `set_theme` no-ops once the theme already matches, so the
        // per-cell `theme:changed` echoes terminate immediately.
        for cell in self.cells() {
            if let Some(chart) = cell.try_chart() {
                chart.set_theme(theme.clone());
            }
        }
    }

    /// The current theme spec.
    pub fn theme(&self) -> ThemeSpec {
        self.inner.s.borrow().theme.clone()
    }

    // ── layout ──

    /// The current layout definition.
    pub fn layout(&self) -> LayoutDefinition {
        self.inner.s.borrow().def.clone()
    }

    fn resolve_layout(&self, id: &str) -> Result<LayoutDefinition, WorkspaceError> {
        // Registered ids first, then the picker's self-describing dynamic ids (`g3x2`):
        // those synthesize without touching the registry.
        self.inner
            .layouts
            .ensure(id)
            .ok_or_else(|| WorkspaceError::UnknownLayout(id.to_string()))
    }

    fn backend_for(&self, def: &LayoutDefinition) -> NativeBackend {
        // An explicit backend is the host's word: the WebGL budget policy only decides `Auto`.
        let explicit = self.inner.chart_opts.borrow().native_backend;
        if let Some(b) = explicit
            && b != NativeBackend::Auto
        {
            return b;
        }
        if def.cells.len() > self.inner.max_webgl_cells {
            NativeBackend::Canvas2d
        } else {
            NativeBackend::Auto
        }
    }

    /// Switch the grid to a registered layout (or a picker grid id).
    pub fn set_layout(&self, id: &str) -> Result<(), WorkspaceError> {
        if self.inner.destroyed.get() || self.inner.mono_layout {
            return Ok(()); // single-chart mode: the grid is pinned to '1'
        }
        let def = self.resolve_layout(id)?;
        self.set_layout_def(def);
        Ok(())
    }

    /// Switch the grid. Cells are diffed BY IDENTITY (`order` head of the next size):
    /// surviving cells keep their live charts untouched; cells past the new size dehydrate
    /// into the pool; (re)appearing positions hydrate their identity from the pool (or its
    /// seed). Crossing the WebGL budget rebuilds every cell through the pool so the backend
    /// stays uniform.
    pub fn set_layout_def(&self, next: LayoutDefinition) {
        if self.inner.destroyed.get() || self.inner.mono_layout {
            return;
        }
        self.clear_maximized(); // a maximized cell is presentation: a layout switch restores first
        let next_backend = self.backend_for(&next);
        let rebuild_all = next_backend != self.inner.s.borrow().cell_backend;
        // The ACTIVE chart always survives a shrink: it moves into the last kept slot
        // instead of pooling, so changing the grid never hides the chart the user is
        // working in.
        let (keep, preexisting, doomed) = {
            let mut s = self.inner.s.borrow_mut();
            s.order = order_after_layout(&s.order, next.cells.len(), s.active_id.as_deref());
            let keep: std::collections::HashSet<String> =
                s.order.iter().take(next.cells.len()).cloned().collect();
            let preexisting: std::collections::HashSet<String> = s.cells.keys().cloned().collect();
            let doomed: Vec<String> = s
                .order
                .iter()
                .filter(|id| s.cells.contains_key(*id) && (!keep.contains(*id) || rebuild_all))
                .cloned()
                .collect();
            (keep, preexisting, doomed)
        };
        let _ = keep;
        for id in doomed {
            self.drop_cell(&id, true);
        }
        {
            let mut s = self.inner.s.borrow_mut();
            s.def = next.clone();
            s.cell_backend = next_backend;
        }
        self.apply_grid();
        self.build_cells();
        self.align_new_cell_styles(&preexisting);
        self.align_new_cell_drawings(&preexisting);
        let (nextactive, same) = {
            let s = self.inner.s.borrow();
            let ids: Vec<String> = s.order.iter().take(next.cells.len()).cloned().collect();
            let active = active_after_layout(s.active_id.as_deref(), &ids);
            let same = active == s.active_id;
            (active, same)
        };
        if same {
            self.project_active_cell(); // same slot, maybe a rebuilt cell
        } else {
            self.set_active_cell(nextactive.as_deref());
        }
        self.refresh_retention();
        self.emit(&WorkspaceEvent::LayoutChanged { layout: next.id });
        self.mark_state_dirty();
    }

    /// Destroy one live cell (dehydrating it into the pool first when asked).
    fn drop_cell(&self, id: &str, pool: bool) {
        let cell = self.inner.s.borrow_mut().cells.remove(id);
        let Some(cell) = cell else { return };
        if pool {
            let state = cell.dehydrate();
            self.pool_set(id, state);
        }
        cell.destroy();
        self.inner.host.destroy_cell(id);
        self.inner.replay.cell_destroyed(id);
        self.emit(&WorkspaceEvent::CellDestroyed { id: id.to_string() });
    }

    /// Pool a dehydrated slot state (bounded: the oldest entries drop past the cap).
    fn pool_set(&self, id: &str, state: PooledCellState) {
        let mut s = self.inner.s.borrow_mut();
        s.pool.retain(|(k, _)| k != id);
        s.pool.push((id.to_string(), state));
        if s.pool.len() > POOL_CAP {
            s.pool.remove(0);
        }
    }

    /// The identity of the cell maximized over the whole grid, or `None`.
    pub fn maximized_cell(&self) -> Option<String> {
        self.inner.s.borrow().maximized_id.clone()
    }

    /// Maximize one cell over the whole grid, or restore the layout with `None`. Pure
    /// presentation: the other cells stay alive underneath, so restoring is instant. The
    /// maximized cell becomes the active one. Transient view state: switching layouts or
    /// applying a state document restores the grid.
    pub fn maximize_cell(&self, id: Option<&str>) {
        if self.inner.destroyed.get() {
            return;
        }
        {
            let s = self.inner.s.borrow();
            if let Some(id) = id
                && (!s.cells.contains_key(id) || s.def.cells.len() <= 1)
            {
                return;
            }
            if id == s.maximized_id.as_deref() {
                return;
            }
        }
        self.inner.s.borrow_mut().maximized_id = id.map(str::to_string);
        if let Some(id) = id {
            self.set_active_cell(Some(id));
        }
        self.apply_grid(); // re-derives the slot geometry, then overlays the maximize presentation
        self.emit(&WorkspaceEvent::CellMaximized {
            id: id.map(str::to_string),
        });
        self.project_active_cell();
    }

    /// Drop the transient maximize on a structural change, WITH the event, so hosts
    /// tracking `cell:maximized` never drift from `maximized_cell`.
    fn clear_maximized(&self) {
        let had = self.inner.s.borrow_mut().maximized_id.take().is_some();
        if had {
            self.emit(&WorkspaceEvent::CellMaximized { id: None });
        }
    }

    /// Mark one cell as the live drop target of a grip drag (`None` clears).
    pub fn set_drop_target(&self, id: Option<&str>) {
        self.inner.s.borrow_mut().drop_target = id.map(str::to_string);
        self.present_grid();
    }

    /// Trade the SLOTS of two live cells: the grid arrangement changes, the cells
    /// themselves (charts, indicators, drawings, the active flag) stay untouched. What each
    /// cell's drag handle commits; also callable directly by hosts.
    pub fn swap_cells(&self, a: &str, b: &str) {
        if self.inner.destroyed.get() || a == b {
            return;
        }
        {
            let mut s = self.inner.s.borrow_mut();
            let (Some(i), Some(j)) = (
                s.order.iter().position(|x| x == a),
                s.order.iter().position(|x| x == b),
            ) else {
                return;
            };
            if !s.cells.contains_key(a) || !s.cells.contains_key(b) {
                return;
            }
            s.order.swap(i, j);
        }
        self.apply_grid();
        self.mark_state_dirty();
    }

    // ── tracks (splitters) ──

    /// The current track weights per axis (the layout's own, unless a drag overrode them).
    pub fn tracks(&self) -> (Vec<f64>, Vec<f64>) {
        let s = self.inner.s.borrow();
        let sizes = s.track_sizes.get(&s.def.id);
        let cols = match sizes.and_then(|t| t.cols.as_ref()) {
            Some(c) if c.len() == s.def.cols.len() => c.clone(),
            _ => s.def.cols.clone(),
        };
        let rows = match sizes.and_then(|t| t.rows.as_ref()) {
            Some(r) if r.len() == s.def.rows.len() => r.clone(),
            _ => s.def.rows.clone(),
        };
        (cols, rows)
    }

    /// The splitter strips for the current grid in a container of `width` x `height` CSS px.
    pub fn splitter_strips(&self, width: f64, height: f64) -> Vec<SplitterStrip> {
        let (cols, rows) = self.tracks();
        let grid = occupancy_grid(&self.inner.s.borrow().def);
        splitter_strips(&cols, &rows, &grid, width, height, GAP_PX)
    }

    /// The grid gap in px (the strips center on the gaps).
    pub fn gap_px(&self) -> f64 {
        GAP_PX
    }

    /// Commit new track weights for one axis (a splitter drag). The workspace re-applies
    /// the grid template.
    pub fn apply_tracks(&self, axis: Axis, weights: Vec<f64>) {
        {
            let mut s = self.inner.s.borrow_mut();
            let id = s.def.id.clone();
            let sizes = s.track_sizes.entry(id).or_default();
            match axis {
                Axis::Cols => sizes.cols = Some(weights),
                Axis::Rows => sizes.rows = Some(weights),
            }
        }
        self.apply_grid();
        self.mark_state_dirty();
    }

    /// Reset one axis to an even split (a splitter double-click).
    pub fn reset_tracks(&self, axis: Axis) {
        let (cols, rows) = self.tracks();
        let n = match axis {
            Axis::Cols => cols.len(),
            Axis::Rows => rows.len(),
        };
        self.apply_tracks(axis, even_tracks(n));
    }

    /// The grid as the host should show it right now.
    pub fn grid_presentation(&self) -> GridPresentation {
        let (cols, rows) = self.tracks();
        let s = self.inner.s.borrow();
        let styles = grid_styles(&s.def, s.track_sizes.get(&s.def.id));
        let maximized = s.maximized_id.clone();
        let cells = s
            .def
            .cells
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                let id = s.order.get(i)?;
                s.cells.get(id)?;
                let own_area = styles.per_cell.get(&slot.id).cloned().flatten();
                let is_max = maximized.as_deref() == Some(id.as_str());
                Some(CellPlacement {
                    id: id.clone(),
                    slot: i,
                    slot_id: slot.id.clone(),
                    // The siblings must span too: left in their slots they would auto-flow
                    // into implicit zero-height rows.
                    grid_area: if maximized.is_some() {
                        Some("1 / 1 / -1 / -1".into())
                    } else {
                        own_area
                    },
                    hidden: maximized.is_some() && !is_max,
                    z_index: is_max.then_some(5),
                    active: s.active_id.as_deref() == Some(id.as_str()),
                    drop_target: s.drop_target.as_deref() == Some(id.as_str()),
                })
            })
            .collect();
        GridPresentation {
            layout: s.def.id.clone(),
            cols,
            rows,
            areas: styles.areas.clone(),
            multi: s.def.cells.len() > 1,
            maximized,
            cells,
            gap_px: GAP_PX,
        }
    }

    fn present_grid(&self) {
        let grid = self.grid_presentation();
        self.inner.host.present_grid(&grid);
    }

    fn apply_grid(&self) {
        self.present_grid();
    }

    // ── cells ──

    fn seed_for(&self, id: &str) -> CellBoot {
        let base = seed_defaults(&self.inner.chart_opts.borrow());
        let own = self
            .inner
            .seeds
            .iter()
            .find(|(n, _)| n == id)
            .map(|(_, seed)| seed.clone())
            .unwrap_or_default();
        own.over(base).into()
    }

    /// Create the cells the current layout wants but don't exist yet (pool-first). A slot's
    /// CELL IDENTITY is `order[i]` (declaration order, never the slot's own positional id);
    /// slots past the declared list mint an auto identity once.
    fn build_cells(&self) {
        let slots = self.inner.s.borrow().def.cells.len();
        let observer: Weak<dyn CellObserver> = self.inner.me.clone() as Weak<WsInner>;
        for i in 0..slots {
            let id = {
                let mut s = self.inner.s.borrow_mut();
                match s.order.get(i).cloned() {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        let mut taken: std::collections::HashSet<String> =
                            s.order.iter().cloned().collect();
                        taken.extend(s.pool.iter().map(|(k, _)| k.clone()));
                        taken.extend(s.cells.keys().cloned());
                        let id = next_auto_cell_id(&taken);
                        if s.order.len() <= i {
                            s.order.resize(i + 1, String::new());
                        }
                        s.order[i] = id.clone();
                        id
                    }
                }
            };
            if self.inner.s.borrow().cells.contains_key(&id) {
                continue;
            }
            let pooled = {
                let mut s = self.inner.s.borrow_mut();
                let at = s.pool.iter().position(|(k, _)| *k == id);
                at.map(|i| s.pool.remove(i).1) // the slot is live again: its pooled state is consumed
            };
            let fresh = pooled.is_none();
            let had_ledger = pooled.as_ref().is_some_and(|p| p.indicators.is_some());
            let boot = match pooled {
                Some(state) => CellBoot::from(state),
                None => self.seed_for(&id),
            };
            let native = self.inner.host.create_cell(&id);
            let (theme, backend, manifest_empty) = {
                let s = self.inner.s.borrow();
                (s.theme.clone(), s.cell_backend, s.manifest.is_empty())
            };
            let _ = (fresh, manifest_empty);
            let deps = CellDeps {
                observer: observer.clone(),
                env: self.inner.env.clone(),
                native_host: native,
                feed: self.inner.feed.clone(),
                engines: self.inner.engines.clone(),
                chart_defaults: cell_chart_defaults(&self.inner.chart_opts.borrow()),
                localization: self.inner.locale.borrow().options().clone(),
                theme,
                live: self.inner.chart_opts.borrow().live.unwrap_or(false),
                volume: self.inner.chart_opts.borrow().volume.unwrap_or(true),
                statusline: self.inner.statusline,
                watermark: self.inner.watermark,
                native_backend: backend,
                ext_handlers: self.inner.ext_cell.clone(),
            };
            let cell = match ChartCell::new(&id, boot, deps) {
                Ok(cell) => cell,
                Err(err) => {
                    self.inner.host.destroy_cell(&id);
                    self.inner
                        .platform
                        .warn(&format!("[vela] cell \"{id}\" could not be built: {err}"));
                    continue;
                }
            };
            self.inner
                .s
                .borrow_mut()
                .cells
                .insert(id.clone(), cell.clone());
            if let Some(renderer) = cell.renderer() {
                self.inner.host.cell_mounted(&id, &renderer);
            }
            self.inner.host.cell_ready(&id, &cell);
            self.wire_cell(&cell);
            // A fresh renderer starts desktop and on its own clock: push the live mode and the
            // host's pulse so touch gestures, dialogs and the countdown agree from the first frame.
            self.apply_shell_presentation(&cell);
            let (favs, has_tools, manifest, tz_ok) = {
                let s = self.inner.s.borrow();
                (
                    s.favs.clone(),
                    !s.tool_defs.is_empty() || !s.tool_templates.is_empty(),
                    s.manifest.clone(),
                    true,
                )
            };
            let _ = tz_ok;
            // The shared star set is a workspace pref: every newborn cell inherits it
            // silently (equal-set idempotence keeps the favorites event from echoing).
            if !favs.is_empty()
                && let Some(chart) = cell.try_chart()
            {
                chart.drawings().set_favorites(&favs);
            }
            // Same for the tools' remembered settings: a new chart's first trend line looks
            // like the last one drawn anywhere in the workspace.
            if has_tools {
                self.push_tool_defaults(None, std::slice::from_ref(&cell));
            }
            // The indicator ledger: a restored cell re-adds ITS recorded set (held until
            // the manifest resolves); a fresh cell seeds the manifest's enabled entries.
            cell.set_manifest(&manifest, !had_ledger);
            // Third-party state last: the cell is wired and its core state is in place.
            cell.restore_persisted_ext();
            self.emit(&WorkspaceEvent::CellCreated { id: id.clone() });
            self.inner.replay.cell_created(&id);
        }
        // Drop unused trailing identities left by an interrupted build.
        self.present_grid();
    }

    fn apply_shell_presentation(&self, cell: &ChartCell) {
        let (mode, clock) = {
            let s = self.inner.s.borrow();
            (s.layout_mode, s.wall_clock.clone())
        };
        if let Some(chart) = cell.try_chart() {
            chart.renderer().set_layout_mode(mode);
            if clock.is_some() {
                chart.renderer().set_wall_clock(clock);
            }
        }
    }

    /// The shell's chrome size class changed (the container crossed the breakpoint, or a
    /// coarse pointer appeared): push it into every cell's renderer, and into cells born
    /// later. The host closes its own open surfaces (pickers, drawers, menus) first.
    pub fn set_layout_mode(&self, mode: LayoutMode) {
        if self.inner.destroyed.get() {
            return;
        }
        self.inner.s.borrow_mut().layout_mode = mode;
        for cell in self.cells() {
            if let Some(chart) = cell.try_chart() {
                chart.renderer().close_dialogs();
                chart.renderer().set_layout_mode(mode);
            }
        }
    }

    /// The current chrome size class.
    pub fn layout_mode(&self) -> LayoutMode {
        self.inner.s.borrow().layout_mode
    }

    /// Hand every cell the host's one second pulse (the bottom-bar clock), so each cell's
    /// countdown chip ticks on the same pulse. `None` hands the pulse back to the renderers.
    pub fn set_wall_clock(&self, clock: Option<Rc<dyn WallClock>>) {
        self.inner.s.borrow_mut().wall_clock = clock.clone();
        for cell in self.cells() {
            if let Some(chart) = cell.try_chart() {
                chart.renderer().set_wall_clock(clock.clone());
            }
        }
    }

    /// Per-cell chart subscriptions (trigger 2: the chart instance is stable for the cell's
    /// whole life, so these live and die with the cell).
    fn wire_cell(&self, cell: &ChartCell) {
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let id = cell.id().to_string();
        let on = |name: ChartEventName, f: ChartHandler| {
            let weak = self.downgrade();
            chart.on(name, move |ev| {
                if let Some(ws) = weak.upgrade() {
                    f(&ws, ev);
                }
            });
        };
        let cid = id.clone();
        on(
            ChartEventName::IndicatorError,
            Box::new(move |ws, ev| {
                if let ChartEvent::IndicatorError { error, .. } = ev {
                    ws.toast(&format!("[{cid}] {error}"), ToastKind::Error, 5000.0);
                }
            }),
        );
        // Script runs relay up with the cell they came from, so ONE subscription on the
        // workspace covers a grid whose cells come and go.
        let cid = id.clone();
        on(
            ChartEventName::ScriptRun,
            Box::new(move |ws, ev| {
                if let ChartEvent::ScriptRun(run) = ev {
                    ws.emit(&WorkspaceEvent::ScriptRun(Box::new(WorkspaceScriptRun {
                        run: run.clone(),
                        cell: cid.clone(),
                    })));
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::PriceStyleChange,
            Box::new(move |ws, ev| {
                if let ChartEvent::PriceStyleChange { from, to } = ev {
                    ws.emit(&WorkspaceEvent::CellPriceStyle {
                        id: cid.clone(),
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::Alert,
            Box::new(move |ws, ev| {
                if let ChartEvent::Alert(alert) = ev {
                    ws.on_alert(&cid, alert);
                }
            }),
        );
        // Favorites are a WORKSPACE preference: one shared toolbar, one star set: a star
        // toggled in any cell re-applies to every other cell, and the mirror is what
        // `get_state()` persists.
        let cid = id.clone();
        on(
            ChartEventName::DrawingFavorites,
            Box::new(move |ws, ev| {
                let ChartEvent::DrawingFavorites { favorites } = ev else {
                    return;
                };
                ws.inner.s.borrow_mut().favs = favorites.clone();
                for other in ws.cells() {
                    if other.id() != cid
                        && let Some(c) = other.try_chart()
                    {
                        c.drawings().set_favorites(favorites);
                    }
                }
                ws.emit(&WorkspaceEvent::FavoritesChanged);
                ws.mark_state_dirty();
            }),
        );
        // The tools' remembered settings are a WORKSPACE preference too.
        let cid = id.clone();
        on(
            ChartEventName::DrawingDefaults,
            Box::new(move |ws, _| ws.on_tool_prefs(&cid, false)),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingTemplates,
            Box::new(move |ws, _| ws.on_tool_prefs(&cid, true)),
        );
        // Crosshair sync: mirror THIS cell's pointer time onto its same-group followers as
        // ghost markers. Leave already emits `time: None`: the clear rides along. The
        // horizontal level travels only when the cursor is on the PRICE pane.
        {
            let weak = self.downgrade();
            let cid = id.clone();
            let _ = chart.renderer().on_crosshair_move(Box::new(move |e| {
                if let Some(ws) = weak.upgrade() {
                    let price = (e.pane_kind == Some(crate::model::PaneKind::Price))
                        .then_some(e.price)
                        .flatten();
                    ws.propagate_crosshair(&cid, e.time, price);
                }
            }));
        }
        // Drawing edits are cell state (the per-slot drawings document): persistable. With
        // the drawings link on, a CREATED drawing copies onto the same-group cells and the
        // set stays LINKED.
        let cid = id.clone();
        on(
            ChartEventName::DrawingCreated,
            Box::new(move |ws, ev| {
                if let ChartEvent::DrawingCreated { id } = ev {
                    ws.propagate_drawing(&cid, id);
                }
                ws.mark_state_dirty();
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingDraft,
            Box::new(move |ws, ev| {
                if let ChartEvent::DrawingDraft { doc } = ev {
                    ws.propagate_draft(&cid, doc.as_ref());
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingEdited,
            Box::new(move |ws, ev| {
                if let ChartEvent::DrawingEdited { id } = ev {
                    ws.propagate_drawing_edit(&cid, id);
                }
                ws.mark_state_dirty();
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingRemoved,
            Box::new(move |ws, ev| {
                if let ChartEvent::DrawingRemoved { id } = ev {
                    ws.propagate_drawing_removal(&cid, id);
                }
                ws.mark_state_dirty();
            }),
        );
        // Tool, magnet and mode reflection: the ACTIVE cell's drawing state drives the
        // global mirrors, whatever the source.
        let cid = id.clone();
        on(
            ChartEventName::DrawingTool,
            Box::new(move |ws, ev| {
                if ws.active_id().as_deref() != Some(cid.as_str()) {
                    return;
                }
                if let ChartEvent::DrawingTool { type_key } = ev {
                    ws.inner.s.borrow_mut().global_tool = type_key.clone();
                    ws.emit(&WorkspaceEvent::DrawingToolsChanged);
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingSnap,
            Box::new(move |ws, ev| {
                if ws.active_id().as_deref() != Some(cid.as_str()) {
                    return;
                }
                if let ChartEvent::DrawingSnap { mode } = ev {
                    let changed = {
                        let mut s = ws.inner.s.borrow_mut();
                        let changed = *mode != s.global_snap;
                        s.global_snap = *mode;
                        changed
                    };
                    ws.emit(&WorkspaceEvent::DrawingToolsChanged);
                    if changed {
                        ws.mark_state_dirty();
                    }
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingStay,
            Box::new(move |ws, ev| {
                if ws.active_id().as_deref() != Some(cid.as_str()) {
                    return;
                }
                if let ChartEvent::DrawingStay { on } = ev {
                    let changed = {
                        let mut s = ws.inner.s.borrow_mut();
                        let changed = *on != s.global_stay;
                        s.global_stay = *on;
                        changed
                    };
                    ws.emit(&WorkspaceEvent::DrawingToolsChanged);
                    if changed {
                        ws.mark_state_dirty();
                    }
                }
            }),
        );
        let cid = id.clone();
        on(
            ChartEventName::DrawingMode,
            Box::new(move |ws, _| {
                if ws.active_id().as_deref() == Some(cid.as_str()) {
                    ws.emit(&WorkspaceEvent::DrawingToolsChanged);
                }
            }),
        );
        // Viewport sync: every applied pan/zoom/fit propagates to the same-group cells.
        let cid = id.clone();
        on(
            ChartEventName::ViewportChanged,
            Box::new(move |ws, ev| {
                if let ChartEvent::ViewportChanged { from, to } = ev {
                    ws.propagate_viewport(&cid, (*from as f64, *to as f64));
                }
            }),
        );
        // Style sync: any committed config edit (settings dialog, apply_config) mirrors
        // this cell's style-link slice onto its same-group followers.
        {
            let weak = self.downgrade();
            let cid = id.clone();
            let _ = chart.renderer().on_config_changed(Box::new(move || {
                if let Some(ws) = weak.upgrade() {
                    ws.propagate_style_prefs(&cid);
                }
            }));
        }
        // A theme picked in ONE cell re-skins the WHOLE workspace.
        on(
            ChartEventName::ThemeChanged,
            Box::new(move |ws, ev| {
                if let ChartEvent::ThemeChanged(t) = ev {
                    ws.set_theme(t.clone());
                }
            }),
        );
        // The undo timeline of the ACTIVE cell drives the chrome's undo and redo state.
        {
            let weak = self.downgrade();
            let cid = id.clone();
            let _ = cell.history().on_change(move || {
                if let Some(ws) = weak.upgrade() {
                    ws.project_cell(&cid);
                }
            });
        }
        // Pane collapse and maximize in the ACTIVE cell: re-project the chrome.
        let cid = id;
        on(
            ChartEventName::PaneChanged,
            Box::new(move |ws, _| {
                if ws.active_id().as_deref() == Some(cid.as_str()) {
                    ws.project_cell(&cid);
                }
            }),
        );
    }

    fn on_tool_prefs(&self, cell_id: &str, templates: bool) {
        if self.inner.tool_defs_busy.get() {
            return;
        }
        let Some(cell) = self.cell(cell_id) else {
            return;
        };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        {
            let mut s = self.inner.s.borrow_mut();
            if templates {
                s.tool_templates = chart
                    .drawings()
                    .tool_templates()
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            v.iter()
                                .map(|t| {
                                    serde_json::json!({
                                        "name": t.name,
                                        "settings": serde_json::to_value(&t.settings).unwrap_or(Value::Null),
                                    })
                                })
                                .collect(),
                        )
                    })
                    .collect();
            } else {
                s.tool_defs = chart
                    .drawings()
                    .tool_defaults()
                    .into_iter()
                    .filter_map(|(k, v)| Some((k, serde_json::to_value(v).ok()?)))
                    .collect();
            }
        }
        self.push_tool_defaults(Some(&cell), &self.cells());
        self.emit(&WorkspaceEvent::DrawingToolsChanged);
        self.mark_state_dirty();
    }

    /// Mirror the shared remembered settings onto every cell but `from` (the cell they came
    /// from). Receiving them is not an edit: the cells' `drawing:defaults` echoes are
    /// ignored.
    fn push_tool_defaults(&self, from: Option<&ChartCell>, cells: &[ChartCell]) {
        let (defs, templates) = {
            let s = self.inner.s.borrow();
            (
                Value::Object(s.tool_defs.clone().into_iter().collect()),
                Value::Object(
                    s.tool_templates
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::Array(v.clone())))
                        .collect(),
                ),
            )
        };
        self.inner.tool_defs_busy.set(true);
        for other in cells {
            if from.is_some_and(|f| f.id() == other.id()) {
                continue;
            }
            if let Some(chart) = other.try_chart() {
                chart.drawings().set_tool_defaults(&defs);
                chart.drawings().set_tool_templates(&templates);
            }
        }
        self.inner.tool_defs_busy.set(false);
    }

    fn adopt_tools(s: &mut State, tools: &DrawingToolsState) {
        if let Some(d) = &tools.defaults {
            s.tool_defs = d.clone();
        }
        if let Some(t) = &tools.templates {
            s.tool_templates = t.clone();
        }
        if let Some(m) = tools.magnet {
            s.global_snap = match m {
                Magnet::Off => crate::drawings::SnapMode::Off,
                Magnet::Weak => crate::drawings::SnapMode::Weak,
                Magnet::Strong => crate::drawings::SnapMode::Strong,
            };
        }
        if let Some(stay) = tools.stay {
            s.global_stay = stay;
        }
    }

    fn drawing_tools_state(&self) -> Option<DrawingToolsState> {
        let s = self.inner.s.borrow();
        let mut out = DrawingToolsState::default();
        let mut any = false;
        if !s.tool_defs.is_empty() {
            out.defaults = Some(s.tool_defs.clone());
            any = true;
        }
        if !s.tool_templates.is_empty() {
            out.templates = Some(s.tool_templates.clone());
            any = true;
        }
        if s.global_snap != crate::drawings::SnapMode::Off {
            out.magnet = Some(match s.global_snap {
                crate::drawings::SnapMode::Off => Magnet::Off,
                crate::drawings::SnapMode::Weak => Magnet::Weak,
                crate::drawings::SnapMode::Strong => Magnet::Strong,
            });
            any = true;
        }
        if s.global_stay {
            out.stay = Some(true);
            any = true;
        }
        any.then_some(out)
    }

    fn on_alert(&self, cell_id: &str, alert: &crate::events::types::AlertEvent) {
        let Some(cell) = self.cell(cell_id) else {
            return;
        };
        // Provenance the way a user reads it: symbol, timeframe, and the indicator that
        // fired, captured at fire time (the cell may switch markets later).
        let locale = self.i18n();
        let symbol = cell.symbol();
        let ticker = crate::data::provider_registry::parse_symbol(&symbol).ticker;
        let source = [
            if ticker.is_empty() { symbol } else { ticker },
            timeframe_label(&cell.timeframe(), Some(locale.as_ref())),
            alert.indicator.clone().unwrap_or_default(),
        ]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let title = alert
            .alert
            .title
            .clone()
            .unwrap_or_else(|| self.t(&Text::alert(), &[]));
        let cap = self.inner.alert_cap;
        {
            let mut s = self.inner.s.borrow_mut();
            s.alerts.insert(
                0,
                WorkspaceAlert {
                    cell_id: cell_id.to_string(),
                    source,
                    title,
                    message: alert.alert.message.clone(),
                    time: alert.alert.time,
                },
            );
            s.alerts.truncate(cap);
        }
        self.emit(&WorkspaceEvent::AlertsChanged);
    }

    /// The alerts the bell keeps, newest first.
    pub fn alerts(&self) -> Vec<WorkspaceAlert> {
        self.inner.s.borrow().alerts.clone()
    }

    // ── shared chrome projection ──

    /// What the shared chrome reads from the ACTIVE cell (`None` before the first cell).
    pub fn projection(&self) -> Option<ActiveProjection> {
        let cell = self.active()?;
        let s = self.inner.s.borrow();
        let magnet = match s.global_snap {
            crate::drawings::SnapMode::Off => Magnet::Off,
            crate::drawings::SnapMode::Weak => Magnet::Weak,
            crate::drawings::SnapMode::Strong => Magnet::Strong,
        };
        let projection = ActiveProjection {
            cell_id: cell.id().to_string(),
            symbol: cell.symbol(),
            timeframe: cell.timeframe(),
            price_style: cell.price_style(),
            unavailable_price_styles: cell.unavailable_price_styles(),
            indicator_count: cell.indicator_count(),
            can_undo: cell.history().can_undo(),
            can_redo: cell.history().can_redo(),
            active_range_id: cell.active_range_id(),
            session: cell.session(),
            session_available: cell.session_available(),
            timezone_choice: s.timezone.clone(),
            exchange_timezone: cell.exchange_timezone(),
            drawing_tool: s.global_tool.clone(),
            magnet,
            stay: s.global_stay,
            favorites: s.favs.clone(),
            maximized: s.maximized_id.clone(),
        };
        Some(projection)
    }

    /// Re-project the shared chrome from the ACTIVE cell. The chrome holds no state of its
    /// own: this is a pure read of the cell, safe to call redundantly. Re-applies the
    /// GLOBAL tool, magnet and stay mode to the cell taking focus.
    fn project_active_cell(&self) {
        let Some(cell) = self.active() else { return };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let (tool, snap, stay) = {
            let s = self.inner.s.borrow();
            (s.global_tool.clone(), s.global_snap, s.global_stay)
        };
        let d = chart.drawings();
        if d.get_tool() != tool {
            d.set_tool(tool.as_deref());
        }
        if d.get_snap_mode() != snap {
            d.set_snap_mode(snap);
        }
        if d.get_stay_mode() != stay {
            d.set_stay_mode(stay);
        }
        self.project_cell(cell.id());
    }

    /// A cell's own state changed: re-project only while that cell IS the active one.
    fn project_cell(&self, id: &str) {
        if self.inner.s.borrow().active_id.as_deref() == Some(id) {
            self.emit(&WorkspaceEvent::Projection { id: id.to_string() });
        }
    }

    /// The layout dropdown's sync switches (the simple all-cells form).
    pub fn sync_rows(&self) -> Vec<SyncRow> {
        let s = self.inner.s.borrow();
        let on = |k: SyncKind| matches!(s.sync.get(k), Some(SyncSetting::Flag(true)));
        let checked = [
            ("symbol", on(SyncKind::Symbol), Text::sync_symbol()),
            ("timeframe", on(SyncKind::Timeframe), Text::sync_interval()),
            ("crosshair", on(SyncKind::Crosshair), Text::sync_crosshair()),
            ("style", on(SyncKind::Style), Text::sync_style()),
        ];
        drop(s);
        checked
            .into_iter()
            .map(|(id, checked, label)| SyncRow {
                id,
                label: self.t(&label, &[]),
                checked,
            })
            .collect()
    }

    /// Flip one sync switch of the layout dropdown: it OVERRIDES a host-set group record
    /// with plain on/off (groups stay an API-only shape).
    pub fn toggle_sync(&self, kind: SyncKind) {
        let on = matches!(
            self.inner.s.borrow().sync.get(kind),
            Some(SyncSetting::Flag(true))
        ) || self
            .inner
            .s
            .borrow()
            .sync
            .get(kind)
            .is_some_and(|s| !matches!(s, SyncSetting::Flag(false)));
        self.sync_set(kind, Some(SyncSetting::Flag(!on)));
    }

    // ── shared timeframe favorites ──

    /// Star or unstar a timeframe: a WORKSPACE preference (one chip row, whatever the
    /// active cell); it persists with the document.
    pub fn set_timeframe_favorite(&self, tf: &str, on: bool) {
        {
            let mut s = self.inner.s.borrow_mut();
            if on == s.tf_favs.iter().any(|f| f == tf) {
                return;
            }
            s.tf_favs = if on {
                std::iter::once(tf.to_string())
                    .chain(s.tf_favs.iter().cloned())
                    .collect()
            } else {
                s.tf_favs.iter().filter(|f| *f != tf).cloned().collect()
            };
        }
        self.emit(&WorkspaceEvent::FavoritesChanged);
        self.mark_state_dirty();
    }

    pub fn timeframe_favorites(&self) -> Vec<String> {
        self.inner.s.borrow().tf_favs.clone()
    }

    /// The favorite drawing tools (one star set, every cell).
    pub fn favorites(&self) -> Vec<String> {
        self.inner.s.borrow().favs.clone()
    }

    /// The docked panels' slice of the state document (the dock is the browser's: it reads
    /// this at boot and writes it back through [`set_panels_state`](Self::set_panels_state)).
    pub fn panels_state(&self) -> Option<PanelsState> {
        self.inner.s.borrow().panels.clone()
    }

    pub fn set_panels_state(&self, panels: Option<PanelsState>) {
        self.inner.s.borrow_mut().panels = panels;
        self.mark_state_dirty();
    }

    /// Set the shared manifest once it resolves; every cell seeds its enabled entries (a
    /// restored ledger re-adds by name).
    pub fn set_manifest(&self, list: Vec<ManifestIndicator>) {
        if self.inner.destroyed.get() {
            return;
        }
        {
            let mut s = self.inner.s.borrow_mut();
            s.manifest = list.clone();
            s.manifest_settled = true;
        }
        for cell in self.cells() {
            cell.set_manifest(&list, true);
        }
        self.project_active_cell();
    }

    // ── sync links ──

    /// The current setting of one link.
    pub fn sync_get(&self, kind: SyncKind) -> Option<SyncSetting> {
        self.inner.s.borrow().sync.get(kind).cloned()
    }

    /// Every link's setting.
    pub fn sync_state(&self) -> SyncOptions {
        self.inner.s.borrow().sync.clone()
    }

    /// Set one link: `Some(Flag(true))` = all cells, `Some(Groups)` = `{cell: group}`,
    /// `None` or `Some(Flag(false))` = off. Enabling a market or viewport link aligns the
    /// followers to the ACTIVE cell once, so the grid starts coherent.
    pub fn sync_set(&self, kind: SyncKind, setting: Option<SyncSetting>) {
        self.apply_sync_setting(kind, setting, true);
    }

    fn apply_sync_setting(&self, kind: SyncKind, setting: Option<SyncSetting>, align: bool) {
        let setting = match setting {
            None | Some(SyncSetting::Flag(false)) => None,
            other => other,
        };
        self.inner.s.borrow_mut().sync.set(kind, setting.clone());
        match kind {
            SyncKind::Drawings => {
                // Placement ghosts are stale under the new setting. Link pairs stay: turning
                // off freezes propagation; turning on resumes edit/delete for drawings
                // paired earlier and links identical copies the way a reload does.
                for cell in self.cells() {
                    if let Some(chart) = cell.try_chart() {
                        chart.drawings().set_external_ghost(None);
                    }
                }
                self.relink_identical_drawings();
                self.emit(&WorkspaceEvent::SyncChanged);
                self.mark_state_dirty();
            }
            SyncKind::Crosshair => {
                // Any setting change invalidates current ghosts: they rebuild on the next
                // pointer move under the NEW grouping.
                for cell in self.cells() {
                    if let Some(chart) = cell.try_chart() {
                        chart.renderer().set_external_crosshair(None, None);
                    }
                }
                self.emit(&WorkspaceEvent::SyncChanged);
                self.mark_state_dirty();
            }
            _ => {
                self.emit(&WorkspaceEvent::SyncChanged);
                self.mark_state_dirty();
                let active = self.active_id();
                if align
                    && setting.is_some()
                    && let Some(active) = active
                {
                    match kind {
                        SyncKind::Viewport => {
                            let range = self
                                .cell(&active)
                                .and_then(|c| c.try_chart())
                                .and_then(|c| c.get_visible_range());
                            if let Some(r) = range {
                                self.propagate_viewport(&active, (r.from as f64, r.to as f64));
                            }
                        }
                        SyncKind::Style => self.propagate_style_prefs(&active),
                        _ => self.propagate_market(&active),
                    }
                }
            }
        }
    }

    /// Every live cell id, in slot order first and any stray after (deterministic).
    fn live_ids_in_order(&self) -> Vec<String> {
        let s = self.inner.s.borrow();
        let mut ids: Vec<String> = s
            .order
            .iter()
            .filter(|id| s.cells.contains_key(*id))
            .cloned()
            .collect();
        let mut stray: Vec<String> = s
            .cells
            .keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        stray.sort();
        ids.extend(stray);
        ids
    }

    fn cell_ids(&self) -> Vec<String> {
        // Cell enumeration for sync targets: insertion order of the order list, live cells.
        self.cells().iter().map(|c| c.id().to_string()).collect()
    }

    /// Align cells minted by a layout change to their style group: with the link on, a NEW
    /// cell (fresh slot or one returning from the pool, which missed edits while dormant)
    /// inherits the presentation of a pre-existing group peer: the active cell when it is
    /// one. Propagation runs FROM the peer, so a newborn's defaults never overwrite the
    /// group.
    fn align_new_cell_styles(&self, preexisting: &std::collections::HashSet<String>) {
        let Some(setting) = self.inner.s.borrow().sync.style.clone() else {
            return;
        };
        let ids = self.cell_ids();
        let mut propagated: Vec<String> = Vec::new();
        for id in &ids {
            if preexisting.contains(id) {
                continue;
            }
            let peers: Vec<String> = sync_targets(id, Some(&setting), &ids)
                .into_iter()
                .filter(|p| preexisting.contains(p))
                .collect();
            if peers.is_empty() {
                continue; // an unlinked or all-new group has no source to inherit
            }
            let active = self.active_id();
            let source = match &active {
                Some(a) if peers.contains(a) => a.clone(),
                _ => peers[0].clone(),
            };
            if propagated.contains(&source) {
                continue; // one propagation already covered this group
            }
            propagated.push(source.clone());
            self.propagate_style_prefs(&source);
        }
    }

    /// Bring cells minted by a layout change into their drawings group: with the link on, a
    /// NEW cell receives the drawings of a pre-existing group peer (the active cell when it
    /// is one). A drawing the arriving cell already holds a linked copy of is refreshed in
    /// place; any other is copied and linked like a freshly synced drawing. The arrival is
    /// the cell's starting state, not an edit: it stays out of the cell's undo timeline.
    fn align_new_cell_drawings(&self, preexisting: &std::collections::HashSet<String>) {
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        let ids = self.cell_ids();
        for id in &ids {
            if preexisting.contains(id) {
                continue;
            }
            let Some(cell) = self.cell(id) else { continue };
            let Some(chart) = cell.try_chart() else {
                continue;
            };
            if !chart.drawings().supported() {
                continue;
            }
            let peers: Vec<String> = sync_targets(id, Some(&setting), &ids)
                .into_iter()
                .filter(|p| preexisting.contains(p))
                .collect();
            if peers.is_empty() {
                continue;
            }
            let active = self.active_id();
            let source_id = match &active {
                Some(a) if peers.contains(a) => a.clone(),
                _ => peers[0].clone(),
            };
            let Some(source) = self.cell(&source_id).and_then(|c| c.try_chart()) else {
                continue;
            };
            self.inner.drawing_sync_busy.set(true);
            let drawings = chart.drawings();
            cell.history().silently(|| {
                let own = drawings.all();
                let held: std::collections::HashSet<String> =
                    own.iter().map(|d| d.id.clone()).collect();
                // Unlinked drawings this chart already holds, by content: a chart back from
                // a document saved in an earlier session carries identical copies that only
                // lost their link: adopt those instead of duplicating them.
                let mut unlinked: Vec<(String, Vec<String>)> = Vec::new();
                for d in &own {
                    if self.link_group(id, &d.id).is_some() {
                        continue;
                    }
                    let content = drawing_link_content(d);
                    match unlinked.iter_mut().find(|(c, _)| *c == content) {
                        Some((_, q)) => q.push(d.id.clone()),
                        None => unlinked.push((content, vec![d.id.clone()])),
                    }
                }
                for doc in source.drawings().all() {
                    let group = self.link_group(&source_id, &doc.id).unwrap_or_else(|| {
                        Rc::new(RefCell::new(BTreeMap::from([(
                            source_id.clone(),
                            doc.id.clone(),
                        )])))
                    });
                    let peer_id = group.borrow().get(id).cloned();
                    if let Some(peer) = &peer_id
                        && held.contains(peer)
                    {
                        drawings.update(peer, update_of(&doc));
                        continue;
                    }
                    let content = drawing_link_content(&doc);
                    let matched = unlinked
                        .iter_mut()
                        .find(|(c, _)| *c == content)
                        .and_then(|(_, q)| (!q.is_empty()).then(|| q.remove(0)));
                    if let Some(matched) = matched {
                        if let Some(peer) = &peer_id {
                            self.unlink(id, peer);
                        }
                        group.borrow_mut().insert(id.clone(), matched);
                        self.register_group(&group);
                        continue;
                    }
                    let Some(copy) = drawings.add(&doc.type_key, add_of(&doc, true)) else {
                        continue;
                    };
                    if let Some(peer) = &peer_id {
                        self.unlink(id, peer);
                    }
                    group.borrow_mut().insert(id.clone(), copy.id.clone());
                    self.register_group(&group);
                }
            });
            self.inner.drawing_sync_busy.set(false);
        }
    }

    fn link_key(cell_id: &str, drawing_id: &str) -> String {
        format!("{cell_id}\u{0}{drawing_id}")
    }

    fn link_group(
        &self,
        cell_id: &str,
        drawing_id: &str,
    ) -> Option<Rc<RefCell<BTreeMap<String, String>>>> {
        self.inner
            .s
            .borrow()
            .drawing_links
            .get(&Self::link_key(cell_id, drawing_id))
            .cloned()
    }

    fn unlink(&self, cell_id: &str, drawing_id: &str) {
        self.inner
            .s
            .borrow_mut()
            .drawing_links
            .remove(&Self::link_key(cell_id, drawing_id));
    }

    fn register_group(&self, group: &Rc<RefCell<BTreeMap<String, String>>>) {
        let members: Vec<(String, String)> = group
            .borrow()
            .iter()
            .map(|(c, d)| (c.clone(), d.clone()))
            .collect();
        let mut s = self.inner.s.borrow_mut();
        for (cell_id, d_id) in members {
            s.drawing_links
                .insert(Self::link_key(&cell_id, &d_id), group.clone());
        }
    }

    /// Rebuild the drawings link from content. Which drawings are linked copies lives only
    /// in memory, so a reload or `apply_state` restores every chart's drawings unlinked:
    /// here, drawings with identical content on charts of the same link group become one
    /// linked set again, so documents saved by any earlier session relink without a stored
    /// pairing. A copy edited while the link was off no longer matches and stays
    /// independent. Drawings already linked keep their links.
    fn relink_identical_drawings(&self) {
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        let ids = self.cell_ids();
        for group in sync_groups(Some(&setting), &ids) {
            let held: Vec<(String, Vec<SerializedDrawing>)> = group
                .iter()
                .map(|cell_id| {
                    let drawings = self
                        .cell(cell_id)
                        .and_then(|c| c.try_chart())
                        .map(|c| c.drawings().all())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|d| self.link_group(cell_id, &d.id).is_none())
                        .collect();
                    (cell_id.clone(), drawings)
                })
                .collect();
            let cells: Vec<CellDrawings<'_>> = held
                .iter()
                .map(|(cell_id, drawings)| CellDrawings {
                    cell_id: cell_id.clone(),
                    drawings,
                })
                .collect();
            for link in pair_identical_drawings(&cells) {
                let group = Rc::new(RefCell::new(link.into_iter().collect::<BTreeMap<_, _>>()));
                self.register_group(&group);
            }
        }
    }

    /// Mirror an origin cell's presentation (the style-link slice of its renderer config
    /// plus its Status line and watermark prefs) onto its same-group followers. Loop-safe
    /// two ways: the busy guard eats the followers' SYNCHRONOUS echoes, and the equality
    /// short-circuits leave already-converged followers untouched.
    fn propagate_style_prefs(&self, origin_id: &str) {
        if self.inner.style_sync_busy.get() || self.inner.destroyed.get() {
            return;
        }
        let setting = self.inner.s.borrow().sync.style.clone();
        let targets = sync_targets(origin_id, setting.as_ref(), &self.cell_ids());
        if targets.is_empty() {
            return;
        }
        let Some(origin) = self.cell(origin_id) else {
            return;
        };
        let Some(origin_chart) = origin.try_chart() else {
            return;
        };
        // A renderer without the config port yields no patch: the cell prefs still mirror.
        let config = origin_chart.renderer().get_config();
        let prefs = origin.status_prefs();
        self.inner.style_sync_busy.set(true);
        for id in targets {
            let Some(cell) = self.cell(&id) else { continue };
            let Some(chart) = cell.try_chart() else {
                continue;
            };
            if let (Some(from), Some(to)) = (&config, chart.renderer().get_config())
                && let Some(patch) = style_config_patch(from, &to)
            {
                chart.renderer().apply_config(&Value::Object(patch));
            }
            cell.apply_status_prefs(&prefs);
        }
        self.inner.style_sync_busy.set(false);
    }

    /// Mirror an origin cell's pointer time onto its same-group followers as GHOST
    /// crosshairs. The horizontal price level rides along ONLY to followers showing the
    /// same ticker: price scales are comparable there, while an origin's price painted on
    /// another market's pane would be noise. No busy guard needed: an external crosshair
    /// never re-emits a crosshair move, so no echo loop can exist.
    fn propagate_crosshair(&self, origin_id: &str, time: Option<i64>, price: Option<f64>) {
        if self.inner.destroyed.get() {
            return;
        }
        let Some(setting) = self.inner.s.borrow().sync.crosshair.clone() else {
            return;
        };
        let origin_ticker = self
            .cell(origin_id)
            .map(|c| crate::data::provider_registry::parse_symbol(&c.symbol()).ticker)
            .unwrap_or_default();
        for id in sync_targets(origin_id, Some(&setting), &self.cell_ids()) {
            let Some(follower) = self.cell(&id) else {
                continue;
            };
            let comparable = !origin_ticker.is_empty()
                && crate::data::provider_registry::parse_symbol(&follower.symbol()).ticker
                    == origin_ticker;
            if let Some(chart) = follower.try_chart() {
                chart
                    .renderer()
                    .set_external_crosshair(time, if comparable { price } else { None });
            }
        }
    }

    /// Push an origin cell's visible range onto its same-group followers. Loop-safe two
    /// ways: the busy guard eats the followers' SYNCHRONOUS echoes, and the half-bar
    /// epsilon short-circuits any async residue: a follower already within half of ITS OWN
    /// bar interval is left alone, so cross-timeframe groups settle instead of oscillating.
    fn propagate_viewport(&self, origin_id: &str, range: (f64, f64)) {
        if self.inner.sync_busy.get() || self.inner.destroyed.get() {
            return;
        }
        let setting = self.inner.s.borrow().sync.viewport.clone();
        let targets = sync_targets(origin_id, setting.as_ref(), &self.cell_ids());
        if targets.is_empty() {
            return;
        }
        self.inner.sync_busy.set(true);
        for id in targets {
            let Some(cell) = self.cell(&id) else { continue };
            let Some(chart) = cell.try_chart() else {
                continue;
            };
            let current = chart.get_visible_range();
            let tf_ms = crate::data::timeframe_to_ms(&cell.timeframe()) as f64;
            let eps = if tf_ms.is_finite() { tf_ms / 2.0 } else { 0.0 };
            if let Some(c) = current
                && ranges_within((c.from as f64, c.to as f64), range, eps)
            {
                continue;
            }
            chart.set_visible_range(crate::ports::chart_renderer::VisibleRange {
                from: range.0 as i64,
                to: range.1 as i64,
            });
        }
        self.inner.sync_busy.set(false);
    }

    /// Copy a freshly created drawing onto the origin's same-group followers (fresh ids
    /// through `drawings.add`: anchors are time and price, so the copy lands at the same
    /// spot whatever the follower shows) and register the whole set as LINKED: edits and
    /// removals of any member follow while the link is on.
    fn propagate_drawing(&self, origin_id: &str, drawing_id: &str) {
        if self.inner.drawing_sync_busy.get() || self.inner.destroyed.get() {
            return;
        }
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        let doc = self
            .cell(origin_id)
            .and_then(|c| c.try_chart())
            .and_then(|c| c.drawings().all().into_iter().find(|d| d.id == drawing_id));
        let Some(doc) = doc else { return };
        self.inner.drawing_sync_busy.set(true);
        let group = Rc::new(RefCell::new(BTreeMap::from([(
            origin_id.to_string(),
            drawing_id.to_string(),
        )])));
        for id in sync_targets(origin_id, Some(&setting), &self.cell_ids()) {
            let copy = self
                .cell(&id)
                .and_then(|c| c.try_chart())
                .and_then(|c| c.drawings().add(&doc.type_key, add_of(&doc, false)));
            if let Some(copy) = copy {
                group.borrow_mut().insert(id, copy.id);
            }
        }
        if group.borrow().len() > 1 {
            self.register_group(&group);
        }
        self.inner.drawing_sync_busy.set(false);
    }

    /// Mirror an in-progress placement (its current ghost, `None` = placement ended) onto
    /// the origin's same-group followers: the live half of the drawings link; the created
    /// copy replaces the ghosts when the placement completes. One-way by contract.
    fn propagate_draft(&self, origin_id: &str, doc: Option<&SerializedDrawing>) {
        if self.inner.destroyed.get() {
            return;
        }
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        for id in sync_targets(origin_id, Some(&setting), &self.cell_ids()) {
            if let Some(chart) = self.cell(&id).and_then(|c| c.try_chart()) {
                chart.drawings().set_external_ghost(doc);
            }
        }
    }

    /// Push a linked drawing's edited CONTENT (anchors, style, text, per-type props) onto
    /// its same-group peers: any member propagates, not just the original.
    fn propagate_drawing_edit(&self, origin_id: &str, drawing_id: &str) {
        if self.inner.drawing_sync_busy.get() || self.inner.destroyed.get() {
            return;
        }
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        let Some(group) = self.link_group(origin_id, drawing_id) else {
            return;
        };
        let doc = self
            .cell(origin_id)
            .and_then(|c| c.try_chart())
            .and_then(|c| c.drawings().all().into_iter().find(|d| d.id == drawing_id));
        let Some(doc) = doc else { return };
        self.inner.drawing_sync_busy.set(true);
        for id in sync_targets(origin_id, Some(&setting), &self.cell_ids()) {
            let peer = group.borrow().get(&id).cloned();
            let Some(peer) = peer else { continue };
            if let Some(chart) = self.cell(&id).and_then(|c| c.try_chart()) {
                chart.drawings().update(&peer, update_of(&doc));
            }
        }
        self.inner.drawing_sync_busy.set(false);
    }

    /// Remove a linked drawing's peers with it. The removed member always leaves its link
    /// group (whatever the setting); peers are only deleted while the link is on. A
    /// propagated peer removal re-enters here under the busy guard and just cleans its own
    /// link key.
    fn propagate_drawing_removal(&self, origin_id: &str, drawing_id: &str) {
        if self.inner.destroyed.get() {
            return;
        }
        let Some(group) = self.link_group(origin_id, drawing_id) else {
            return;
        };
        self.unlink(origin_id, drawing_id);
        group.borrow_mut().remove(origin_id);
        if self.inner.drawing_sync_busy.get() {
            return;
        }
        let Some(setting) = self.inner.s.borrow().sync.drawings.clone() else {
            return;
        };
        self.inner.drawing_sync_busy.set(true);
        for id in sync_targets(origin_id, Some(&setting), &self.cell_ids()) {
            let peer = group.borrow().get(&id).cloned();
            if let Some(peer) = peer
                && let Some(chart) = self.cell(&id).and_then(|c| c.try_chart())
            {
                chart.drawings().remove(&peer);
            }
        }
        self.inner.drawing_sync_busy.set(false);
    }

    /// Push an origin cell's symbol and timeframe onto its same-group followers (fired from
    /// `market:changed`). Convergence comes from IDEMPOTENCE, not the guard: a follower's
    /// own (async) `market:changed` propagates back, but every peer already carries the
    /// value, so the cell setters no-op and the wave dies.
    fn propagate_market(&self, origin_id: &str) {
        if self.inner.sync_busy.get() || self.inner.destroyed.get() {
            return;
        }
        let Some(origin) = self.cell(origin_id) else {
            return;
        };
        let ids = self.cell_ids();
        let (symbol_link, tf_link) = {
            let s = self.inner.s.borrow();
            (s.sync.symbol.clone(), s.sync.timeframe.clone())
        };
        self.inner.sync_busy.set(true);
        let symbol = origin.symbol();
        if !symbol.is_empty() {
            for id in sync_targets(origin_id, symbol_link.as_ref(), &ids) {
                if let Some(c) = self.cell(&id) {
                    c.set_symbol(&symbol);
                }
            }
        }
        // The REQUESTED timeframe links, never a fallback a symbol's source forced; a
        // follower that already requests it keeps whatever fallback its own symbol needs.
        let requested = origin.requested_timeframe();
        for id in sync_targets(origin_id, tf_link.as_ref(), &ids) {
            if let Some(follower) = self.cell(&id)
                && follower.requested_timeframe() != requested
            {
                follower.set_timeframe(&requested);
            }
        }
        self.inner.sync_busy.set(false);
    }

    // ── state surface ──

    /// Snapshot the COMPLETE workspace state as a versioned, serializable document: layout
    /// and splitter sizes, active cell, sync links, timezone, and per slot, live AND
    /// dormant, the market, the renderer's cosmetic config, the user-drawings document and
    /// the indicator ledger. This is what `persist` writes; hosts build custom flows on it.
    pub fn get_state(&self) -> WorkspaceState {
        let (live, pool, order) = {
            let s = self.inner.s.borrow();
            (s.cells.clone(), s.pool.clone(), s.order.clone())
        };
        let mut by_id: Vec<(String, PooledCellState)> = Vec::new();
        for (id, cs) in pool {
            by_id.retain(|(k, _)| *k != id);
            by_id.push((id, cs)); // dormant slots
        }
        for (id, cell) in &live {
            let cs = cell.dehydrate();
            by_id.retain(|(k, _)| k != id);
            by_id.push((id.clone(), cs)); // live slots win
        }
        // The charts array is ORDERED: position i of the document is slot i on restore.
        let mut charts: Vec<ChartState> = Vec::new();
        for id in &order {
            if let Some(at) = by_id.iter().position(|(k, _)| k == id) {
                let (id, cell) = by_id.remove(at);
                charts.push(ChartState { id, cell });
            }
        }
        for (id, cell) in by_id {
            charts.push(ChartState { id, cell }); // pooled strays keep restoring
        }
        let tools = self.drawing_tools_state();
        let global_ext = self.dehydrate_global_ext();
        let s = self.inner.s.borrow();
        let mut state = WorkspaceState::new(s.def.id.clone());
        state.timezone = Some(s.timezone.clone());
        state.sync = Some(s.sync.clone());
        state.charts = charts;
        state.active_cell_id = s.active_id.clone();
        if !s.favs.is_empty() {
            state.favorites = Some(s.favs.clone());
        }
        if !s.tf_favs.is_empty() {
            state.timeframe_favorites = Some(s.tf_favs.clone());
        }
        state.drawing_tools = tools;
        if !s.track_sizes.is_empty() {
            state.track_sizes = Some(s.track_sizes.clone());
        }
        state.panels = s.panels.clone();
        state.ext = global_ext;
        state.version = STATE_VERSION;
        state
    }

    /// Document-level third-party state: fresh global-handler snapshots merged OVER the
    /// preserved entries (a key with no handler this session rides along; a handler
    /// returning `None` withdraws its entry).
    fn dehydrate_global_ext(&self) -> Option<BTreeMap<String, Value>> {
        let mut ext = self.inner.s.borrow().ext_state.clone();
        for h in &self.inner.ext_global {
            match h.serialize(self) {
                Some(v) => {
                    ext.insert(h.key().to_string(), v);
                }
                None => {
                    ext.remove(h.key());
                }
            }
        }
        self.inner.s.borrow_mut().ext_state = ext.clone();
        (!ext.is_empty()).then_some(ext)
    }

    fn restore_global_ext(&self) {
        for h in &self.inner.ext_global {
            let value = self.inner.s.borrow().ext_state.get(h.key()).cloned();
            if let Some(value) = value {
                h.restore(&value, self);
            }
        }
    }

    /// Restore a state document produced by [`get_state`](Self::get_state) (untrusted-safe:
    /// malformed fields are dropped; `doc` is any JSON). See
    /// [`apply_state_doc`](Self::apply_state_doc).
    pub fn apply_state(&self, doc: &Value) {
        if self.inner.destroyed.get() {
            return;
        }
        if let Some(st) = sanitize_state(doc) {
            self.apply_state_doc(st);
        }
    }

    /// Restore an already decoded document. When it matches the live grid one-to-one (same
    /// layout, same ordered slot identities) it is applied IN PLACE: every chart instance
    /// survives (markets switch via `set_market`), so chart references, indicator handles
    /// and event subscriptions stay valid. Any structural difference (layout, slot count,
    /// renamed ids) falls back to the full rebuild. A layout id that is not registered keeps
    /// the current grid (register custom layouts first).
    pub fn apply_state_doc(&self, st: WorkspaceState) {
        if self.inner.destroyed.get() {
            return;
        }
        self.apply_state_doc_inner(st);
        if !self.inner.destroyed.get() {
            self.emit(&WorkspaceEvent::PanelsRestored);
        }
    }

    fn apply_state_doc_inner(&self, st: WorkspaceState) {
        {
            let mut s = self.inner.s.borrow_mut();
            if let Some(f) = &st.favorites {
                s.favs = f.clone(); // newborn cells inherit below (build_cells)
            }
            if let Some(t) = &st.drawing_tools {
                Self::adopt_tools(&mut s, t);
            }
            if let Some(f) = &st.timeframe_favorites {
                s.tf_favs = f.clone();
            }
            // Absent in documents written before the dock existed: those leave the column closed.
            s.panels = st.panels.clone();
        }
        for kind in SyncKind::ALL {
            let setting = st.sync.as_ref().and_then(|o| o.get(kind).cloned());
            self.apply_sync_setting(kind, setting, false);
        }
        {
            let mut s = self.inner.s.borrow_mut();
            s.track_sizes = st.track_sizes.clone().unwrap_or_default();
        }
        let (target_def, live_count, in_place) = {
            let s = self.inner.s.borrow();
            let target = if self.inner.mono_layout {
                s.def.clone()
            } else {
                self.inner
                    .layouts
                    .ensure(&st.layout)
                    .unwrap_or_else(|| s.def.clone())
            };
            let live = s.def.cells.len();
            let in_place = target.id == s.def.id
                && st.charts.len() >= live
                && s.order.len() >= live
                && (0..live)
                    .all(|i| st.charts[i].id == s.order[i] && s.cells.contains_key(&s.order[i]));
            (target, live, in_place)
        };
        let _ = target_def;
        if in_place {
            // IN PLACE: same layout, and the document's leading charts map one-to-one onto
            // the live slots. Live cells rehydrate (chart survives); the document's extra
            // charts refresh the dormant pool. No cell:* / layout:changed events:
            // structurally, nothing happened.
            if st.favorites.is_some() {
                let favs = self.inner.s.borrow().favs.clone();
                for cell in self.cells() {
                    if let Some(chart) = cell.try_chart() {
                        chart.drawings().set_favorites(&favs);
                    }
                }
            }
            if st.drawing_tools.is_some() {
                self.push_tool_defaults(None, &self.cells());
            }
            self.inner.s.borrow_mut().drawing_links.clear(); // the document decides what is linked: rebuilt below
            // Restored slots differ legitimately: with the style link on, an unguarded
            // rehydrate would smear each cell's restored config over its peers.
            self.inner.style_sync_busy.set(true);
            for chart_state in st.charts.iter().take(live_count) {
                if let Some(cell) = self.cell(&chart_state.id) {
                    cell.rehydrate(&chart_state.cell);
                }
            }
            self.inner.style_sync_busy.set(false);
            self.relink_identical_drawings();
            // AFTER the cells: each restored renderer config carries its own zone and the
            // cell mirror adopts it: the document's shell-level field must have the last
            // word when the two disagree.
            if let Some(tz) = &st.timezone {
                self.set_timezone(tz);
            }
            {
                let mut s = self.inner.s.borrow_mut();
                s.pool.clear();
                for c in st.charts.iter().skip(live_count) {
                    s.pool.push((c.id.clone(), c.cell.clone()));
                }
                s.order = st.charts.iter().map(|c| c.id.clone()).collect();
            }
            self.clear_maximized();
            self.apply_grid(); // re-assert the restored track sizes on the unchanged grid
            let next_active = {
                let s = self.inner.s.borrow();
                match &st.active_cell_id {
                    Some(a) if s.cells.contains_key(a) => Some(a.clone()),
                    _ => s.order.first().cloned(),
                }
            };
            if next_active == self.active_id() {
                self.project_active_cell();
            } else {
                self.set_active_cell(next_active.as_deref());
            }
            self.refresh_retention();
            // Document-level third-party state converges to the document (absent = none).
            self.inner.s.borrow_mut().ext_state = st.ext.clone().unwrap_or_default();
            self.restore_global_ext();
            self.mark_state_dirty();
            return;
        }

        if let Some(tz) = &st.timezone {
            self.inner.s.borrow_mut().timezone = tz.clone();
            self.emit(&WorkspaceEvent::TimezoneChanged);
        }
        // Full rebuild from the document: every current slot is replaced by the restored one.
        let live_ids = self.live_ids_in_order();
        for id in live_ids {
            self.drop_cell(&id, false);
        }
        {
            let mut s = self.inner.s.borrow_mut();
            s.pool.clear();
            s.drawing_links.clear(); // the document decides what is linked: rebuilt after the cells
            for c in &st.charts {
                s.pool.push((c.id.clone(), c.cell.clone()));
            }
            s.order = st.charts.iter().map(|c| c.id.clone()).collect(); // the document's arrangement IS the order
        }
        let def = if self.inner.mono_layout {
            None // mono: the grid stays pinned to '1'
        } else {
            self.inner.layouts.ensure(&st.layout)
        };
        {
            let mut s = self.inner.s.borrow_mut();
            if let Some(def) = def {
                s.def = def;
            }
        }
        let backend = self.backend_for(&self.inner.s.borrow().def);
        self.inner.s.borrow_mut().cell_backend = backend;
        self.clear_maximized(); // ditto: the rebuilt grid starts unmaximized
        self.apply_grid();
        self.build_cells();
        self.relink_identical_drawings();
        let next_active = {
            let s = self.inner.s.borrow();
            match &st.active_cell_id {
                Some(a) if s.cells.contains_key(a) => Some(a.clone()),
                _ => s.order.first().cloned(),
            }
        };
        if next_active == self.active_id() {
            self.project_active_cell();
        } else {
            self.set_active_cell(next_active.as_deref());
        }
        self.refresh_retention();
        let layout = self.inner.s.borrow().def.id.clone();
        self.emit(&WorkspaceEvent::LayoutChanged { layout });
        // Same convergence as the in-place branch; the rebuilt cells already ran their own
        // cell-scope restores (build_cells -> restore_persisted_ext).
        self.inner.s.borrow_mut().ext_state = st.ext.clone().unwrap_or_default();
        self.restore_global_ext();
        self.mark_state_dirty();
    }

    // ── persistence plumbing ──

    /// Debounced dirty mark: one `state:changed` (and one storage write in persist mode)
    /// per burst of edits, flushed hard on unload and destroy. Hosts that change
    /// third-party persistable state call this (`ctx.stateChanged()`).
    pub fn mark_state_dirty(&self) {
        if self.inner.destroyed.get() || !self.inner.booted.get() {
            return;
        }
        if let Some(t) = self.inner.state_timer.take() {
            self.inner.platform.clear_timeout(t);
        }
        let weak = self.downgrade();
        let id = self.inner.platform.set_timeout(
            STATE_DEBOUNCE_MS,
            Box::new(move || {
                let Some(ws) = weak.upgrade() else { return };
                ws.inner.state_timer.set(None);
                ws.emit(&WorkspaceEvent::StateChanged);
                ws.persist_now();
            }),
        );
        self.inner.state_timer.set(Some(id));
    }

    /// Push a pending debounced change out NOW, for a teardown or a page unload (the two
    /// moments the 500 ms timer would otherwise never reach). Dropping it was a silent data
    /// loss: the timer carries BOTH halves of the dirty signal, the `state:changed` event
    /// and the storage write.
    ///
    /// Idempotent and re-entrancy safe: a no-op unless a timer is pending; the timer is
    /// cleared FIRST, so a second call does nothing; the state is snapshotted BEFORE the
    /// handlers run, and that snapshot is what gets persisted; `emitting` is held across
    /// the emit, so a handler calling back in cannot re-enter this and recurse.
    pub fn flush_pending_state(&self) {
        if self.inner.emitting.get() {
            return;
        }
        let Some(timer) = self.inner.state_timer.take() else {
            return;
        };
        self.inner.platform.clear_timeout(timer);
        let snapshot = self
            .inner
            .persist_key
            .as_ref()
            .map(|_| encode_state(&self.get_state()));
        self.inner.emitting.set(true);
        self.emit(&WorkspaceEvent::StateChanged);
        self.inner.emitting.set(false);
        if let (Some(snapshot), Some(key), Some(storage)) = (
            snapshot,
            self.inner.persist_key.as_ref(),
            self.inner.storage.as_ref(),
        ) {
            storage.set(key, &snapshot);
        }
    }

    /// What the host runs on a page unload: the same flush as a teardown plus a write.
    pub fn on_unload(&self) {
        self.flush_pending_state();
        self.persist_now();
    }

    /// Write the current state through the storage adapter now (fire and forget).
    pub fn persist_now(&self) {
        if self.inner.destroyed.get() {
            return;
        }
        if let (Some(key), Some(storage)) = (&self.inner.persist_key, &self.inner.storage) {
            storage.set(key, &encode_state(&self.get_state()));
        }
    }

    /// Declare every live cell's symbol to the shared bar cache so one cell's load never
    /// evicts the others' history (multi-symbol retention). Keys are CANONICAL tickers:
    /// resolved through the registry when its indexes are ready, raw until then (refreshed
    /// again on `feed.ready()`).
    fn refresh_retention(&self) {
        let mut symbols: Vec<String> = Vec::new();
        for cell in self.cells() {
            let raw = cell.symbol();
            if raw.is_empty() {
                continue;
            }
            symbols.push(
                self.inner
                    .feed
                    .resolve_symbol(&raw)
                    .map_or(raw, |r| r.ticker),
            );
        }
        self.inner
            .env
            .bar_store
            .borrow_mut()
            .retain(symbols, &self.owner_key());
    }

    /// The bar-store owner key of this workspace: other shells on the page keep their own
    /// declarations.
    fn owner_key(&self) -> String {
        format!("workspace@{:p}", Rc::as_ptr(&self.inner))
    }

    // ── screenshot ──

    /// Live cells the screenshot should include (a maximize hides the siblings).
    fn shot_cells(&self) -> Vec<ChartCell> {
        let maximized = self.maximized_cell();
        self.cells()
            .into_iter()
            .filter(|c| maximized.as_deref().is_none_or(|m| m == c.id()))
            .collect()
    }

    /// PNG data URL of the visible layout: every live cell in its grid slot, or the
    /// maximized cell alone. Both go through the host's compositing surface
    /// ([`WorkspaceHost::screenshot_surface`]): a single visible cell paints its own
    /// [`screenshot_pile`](crate::render::native::NativeRenderer::screenshot_pile) over its
    /// box (the native renderer has no export of its own), and falls back to the renderer's
    /// export only when the host cannot composite.
    pub fn screenshot(&self) -> Option<String> {
        let cells = self.shot_cells();
        match cells.len() {
            0 => None,
            1 => self
                .single_cell_shot(&cells[0])
                .or_else(|| cells[0].try_chart()?.renderer().screenshot()),
            _ => self.composite_layout_shot(&cells),
        }
    }

    /// One cell's pile composited on a surface the size of its box.
    fn single_cell_shot(&self, cell: &ChartCell) -> Option<String> {
        let pile = cell.renderer()?.screenshot_pile()?;
        let b = self.inner.host.cell_box(cell.id())?;
        if b.width <= 0.0 || b.height <= 0.0 {
            return None;
        }
        let (dpr, gap_color) = match self.inner.host.grid_metrics() {
            Some((_, dpr, color)) => (dpr, color),
            None => (1.0, String::new()),
        };
        let frame = LayoutShotFrame {
            width: b.width,
            height: b.height,
            dpr,
            gap_color: if gap_color.is_empty() {
                "#000000".into()
            } else {
                gap_color
            },
        };
        let tile = LayoutShotTile {
            background: Some(pile.background),
            sources: pile.images,
            rect: ShotRect {
                x: 0.0,
                y: 0.0,
                width: b.width,
                height: b.height,
            },
        };
        let (w, h) = frame.backing_size();
        let surface = self.inner.host.screenshot_surface(w, h)?;
        let id = cell.id().to_string();
        composite_layout_screenshot_with(
            surface.context().as_ref(),
            &frame,
            &[tile],
            &|_, phase, dest| surface.paint_overlays(&id, phase, dest),
        )
        .then(|| surface.encode_png())
        .flatten()
    }

    /// Place every cell's raster onto one surface the size of the grid.
    fn composite_layout_shot(&self, cells: &[ChartCell]) -> Option<String> {
        let (grid, dpr, gap_color) = self.inner.host.grid_metrics()?;
        let mut tiles: Vec<LayoutShotTile> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for cell in cells {
            let Some(pile) = cell.renderer().and_then(|r| r.screenshot_pile()) else {
                continue;
            };
            let Some(cell_box) = self.inner.host.cell_box(cell.id()) else {
                continue;
            };
            let Some(rect) = tiles_from_cell_rects(&grid, &[cell_box]).first().copied() else {
                continue;
            };
            tiles.push(LayoutShotTile {
                background: Some(pile.background),
                sources: pile.images,
                rect,
            });
            ids.push(cell.id().to_string());
        }
        let frame = LayoutShotFrame {
            width: grid.width,
            height: grid.height,
            dpr,
            gap_color: if gap_color.is_empty() {
                "#000000".into()
            } else {
                gap_color
            },
        };
        let (w, h) = frame.backing_size();
        let surface = self.inner.host.screenshot_surface(w, h)?;
        let painted = composite_layout_screenshot_with(
            surface.context().as_ref(),
            &frame,
            &tiles,
            &|index, phase, dest| surface.paint_overlays(&ids[index], phase, dest),
        );
        painted.then(|| surface.encode_png()).flatten()
    }

    /// The pixel destinations of the visible cells in a layout screenshot: the plan the
    /// host's compositing follows.
    pub fn screenshot_plan(&self) -> Option<Vec<(String, super::screenshot::DestRect)>> {
        let (grid, dpr, _) = self.inner.host.grid_metrics()?;
        let mut out = Vec::new();
        for cell in self.shot_cells() {
            let b = self.inner.host.cell_box(cell.id())?;
            if let Some(rect) = tiles_from_cell_rects(&grid, &[b]).first() {
                let _: &ShotRect = rect;
                out.push((cell.id().to_string(), dest_rect(rect, dpr)));
            }
        }
        Some(out)
    }

    /// Show a toast over the grid: the same surface the shell's own notices use (alerts,
    /// script errors) and the one contributions reach.
    pub fn toast(&self, message: &str, kind: ToastKind, duration_ms: f64) {
        if self.inner.destroyed.get() {
            return;
        }
        self.inner.host.toast(message, kind, duration_ms);
    }

    // ── lifecycle ──

    /// Tear the workspace down: flush the pending state, dehydrate every cell into the pool
    /// (so [`get_state`](Self::get_state) after `destroy` reports what was on screen),
    /// destroy the cells, the replay and the subscriptions. Idempotent.
    pub fn destroy(&self) {
        if self.inner.destroyed.get() {
            return;
        }
        self.flush_pending_state(); // the user's last edit, before anything is torn down
        self.inner.destroyed.set(true);
        self.inner.replay.destroy();
        let ids = self.live_ids_in_order();
        for id in ids {
            let cell = self.inner.s.borrow_mut().cells.remove(&id);
            let Some(cell) = cell else { continue };
            // Dehydrate into the pool before tearing the cell down, exactly as a layout
            // rebuild does: without it `get_state()` after `destroy()` would report the
            // last APPLIED document rather than what was on screen.
            let state = cell.dehydrate();
            self.pool_set(&id, state);
            cell.destroy();
            self.inner.host.destroy_cell(&id);
        }
        self.inner
            .env
            .bar_store
            .borrow_mut()
            .retain(Vec::<String>::new(), &self.owner_key()); // release OUR declaration only
        self.inner.handlers.borrow_mut().clear();
    }
}

/// The patch a linked edit pushes: exactly the shared content (anchors, style, text,
/// per-type props).
fn update_of(doc: &SerializedDrawing) -> DrawingUpdate {
    DrawingUpdate {
        anchors: Some(doc.anchors.clone()),
        style: Some(doc.style.to_partial()),
        text: doc.text.clone().map(Some),
        props: doc.props.clone(),
        ..DrawingUpdate::default()
    }
}

/// What a linked copy is created with. `with_z` carries the draw order too (a cell
/// arriving from a layout change copies the peer's whole stack).
fn add_of(doc: &SerializedDrawing, with_z: bool) -> AddInit {
    AddInit {
        pane_id: Some(doc.pane_id.clone()),
        anchors: Some(doc.anchors.clone()),
        style: Some(doc.style.to_partial()),
        text: doc.text.clone(),
        props: doc.props.clone(),
        z_index: with_z.then_some(doc.z_index),
    }
}

// ── the cell observer ──────────────────────────────────────────

impl WsInner {
    fn ws(&self) -> Option<Workspace> {
        Some(Workspace {
            inner: self.me.upgrade()?,
        })
    }
}

impl CellObserver for WsInner {
    fn activate(&self, id: &str) {
        if let Some(ws) = self.ws() {
            ws.set_active_cell(Some(id));
        }
    }

    fn timezone(&self) -> String {
        self.s.borrow().timezone.clone()
    }

    fn set_timezone(&self, zone: &str) {
        if let Some(ws) = self.ws() {
            ws.set_timezone(zone);
        }
    }

    fn manifest_settled(&self) -> bool {
        self.s.borrow().manifest_settled
    }

    /// Trigger 2: a cell's market changed: retention and sync always; chrome only if active.
    fn market_changed(&self, id: &str) {
        let Some(ws) = self.ws() else { return };
        ws.refresh_retention();
        ws.propagate_market(id);
        ws.mark_state_dirty();
        ws.project_cell(id);
    }

    fn price_style_changed(&self, id: &str) {
        let Some(ws) = self.ws() else { return };
        ws.mark_state_dirty();
        ws.project_cell(id);
    }

    fn price_style_shown(&self, id: &str) {
        if let Some(ws) = self.ws() {
            ws.project_cell(id);
        }
    }

    fn indicators_changed(&self, id: &str) {
        let Some(ws) = self.ws() else { return };
        ws.mark_state_dirty();
        ws.project_cell(id);
    }

    fn catalog_changed(&self, id: &str) {
        if let Some(ws) = self.ws() {
            ws.project_cell(id);
        }
    }

    fn status_prefs_changed(&self, id: &str) {
        if let Some(ws) = self.ws() {
            ws.emit(&WorkspaceEvent::StatusPrefs { id: id.to_string() });
            ws.propagate_style_prefs(id);
        }
    }

    fn state_dirty(&self) {
        if let Some(ws) = self.ws() {
            ws.mark_state_dirty();
        }
    }

    fn toast(&self, message: &str, kind: ToastKind, duration_ms: f64) {
        if let Some(ws) = self.ws() {
            ws.toast(message, kind, duration_ms);
        }
    }
}

#[cfg(test)]
mod tests;
