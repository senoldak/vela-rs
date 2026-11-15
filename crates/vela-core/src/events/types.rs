//! The chart's event map: every event name and its payload.
//!
//! The original is one TypeScript interface (`VelaEventMap`) whose keys select a
//! payload type. Rust has no such map, so the port is a pair of enums that fit
//! [`TypedEventBus`](super::TypedEventBus):
//!
//! - [`ChartEventName`] is the key `K`: one unit variant per event, with
//!   [`as_str`](ChartEventName::as_str) returning the exact wire name (`bar`,
//!   `indicator:added`, `drawing:created`, ...).
//! - [`ChartEvent`] is the payload `P`: one variant per event carrying its
//!   payload as named fields (or a payload type), with
//!   [`name`](ChartEvent::name) giving the key to emit it under.
//!
//! ```ignore
//! let bus = TypedEventBus::<ChartEventName, ChartEvent>::new();
//! bus.on(ChartEventName::Bar, |event| { /* match event */ });
//! let event = ChartEvent::Bar(bar);
//! bus.emit(&event.name(), &event);
//! ```
//!
//! Differences from the original:
//! - Events without a payload (`ready`, `pane:changed`, `replay:pause`) are unit
//!   variants.
//! - `indicator:error` carries the error's message (`String`), not an `Error`.
//! - `theme:changed` carries the opaque [`VelaTheme`].
//! - `script:run` carries [`ScriptRun`] (ported in [`crate::script_run`], whose
//!   two promise-returning accessors are loader closures held by the value),
//!   and `replay:end` carries [`ReplayEndReason`] (from
//!   [`crate::controls::replay_control`]); both are re-exported here.
//! - A payload key named `type` is `type_key`; `data:unavailable`'s `default` is
//!   the field `default`.
//! - `alert` is `EngineAlert & { indicator?: string }`: [`AlertEvent`] flattens
//!   the alert and adds the optional indicator title.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::data_unavailable::DataUnavailableReason;
use crate::drawings::{SerializedDrawing, SnapMode};
use crate::i18n::LocaleChangedEvent;
use crate::marks::MarkClickEvent;
use crate::model::{Millis, Ohlcv};
use crate::ports::chart_renderer::{DrawingMode, DrawingTypeKey, PaneMoveDir, VelaTheme};
use crate::ports::market_data::SymbolFacts;
use crate::ports::scripting_engine::{EngineAlert, EngineWarning};

pub use crate::controls::replay_control::ReplayEndReason;
pub use crate::script_run::{
    PlotSeriesPoint, ScriptKind, ScriptRun, ScriptRunCause, SeriesLoader, TradesLoader,
};

/// The identity of a chart market: symbol and timeframe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketIdentity {
    pub symbol: String,
    pub timeframe: String,
}

/// How the history load finished (`history:complete`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryReason {
    /// The requested bar count is loaded.
    Depth,
    /// The source has nothing older (full available history).
    Genesis,
    /// A fetch failed or the data was non-monotonic: the chart keeps what loaded.
    Aborted,
}

/// The `alert` payload: the engine's alert plus the indicator that raised it.
/// `indicator` names the source: the indicator's display title (what its legend
/// row shows). Never fires for bars a replay reveals (`chart.replay`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlertEvent {
    #[serde(flatten)]
    pub alert: EngineAlert,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicator: Option<String>,
}

macro_rules! event_names {
    ($( $(#[$meta:meta])* $variant:ident => $name:literal ),+ $(,)?) => {
        /// The name of a chart event: the key of [`TypedEventBus`](super::TypedEventBus).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ChartEventName {
            $( $(#[$meta])* $variant, )+
        }

        impl ChartEventName {
            /// Every event name, in the original's declaration order.
            pub const ALL: &'static [ChartEventName] = &[ $( ChartEventName::$variant, )+ ];

            /// The exact name the original emits it under (`indicator:added`).
            pub fn as_str(self) -> &'static str {
                match self {
                    $( ChartEventName::$variant => $name, )+
                }
            }

            /// The event named `name`, if it is one.
            pub fn from_name(name: &str) -> Option<ChartEventName> {
                match name {
                    $( $name => Some(ChartEventName::$variant), )+
                    _ => None,
                }
            }
        }
    };
}

event_names! {
    /// No payload.
    Ready => "ready",
    MarketChanged => "market:changed",
    LoadStart => "load:start",
    LoadEnd => "load:end",
    IndicatorAdded => "indicator:added",
    IndicatorRemoved => "indicator:removed",
    IndicatorInputs => "indicator:inputs",
    IndicatorError => "indicator:error",
    DataUnresolved => "data:unresolved",
    DataUnavailable => "data:unavailable",
    SymbolFacts => "symbol:facts",
    IndicatorMoved => "indicator:moved",
    IndicatorVisibility => "indicator:visibility",
    /// No payload.
    PaneChanged => "pane:changed",
    ThemeChanged => "theme:changed",
    LocaleChanged => "locale:changed",
    PriceStyleChange => "priceStyle:change",
    PaneMoved => "pane:moved",
    DrawingCreated => "drawing:created",
    DrawingDraft => "drawing:draft",
    DrawingEdited => "drawing:edited",
    DrawingSelected => "drawing:selected",
    DrawingFavorites => "drawing:favorites",
    DrawingDefaults => "drawing:defaults",
    DrawingTemplates => "drawing:templates",
    DrawingTool => "drawing:tool",
    DrawingSnap => "drawing:snap",
    DrawingStay => "drawing:stay",
    DrawingMode => "drawing:mode",
    DrawingRemoved => "drawing:removed",
    DrawingSettings => "drawing:settings",
    MarkClick => "mark:click",
    ScriptRun => "script:run",
    ContextChanged => "context:changed",
    Bar => "bar",
    ViewportChanged => "viewport:changed",
    HistoryProgress => "history:progress",
    HistoryComplete => "history:complete",
    ReplayStart => "replay:start",
    ReplayStep => "replay:step",
    ReplayTick => "replay:tick",
    ReplayPlay => "replay:play",
    /// No payload.
    ReplayPause => "replay:pause",
    ReplayEnd => "replay:end",
    Alert => "alert",
    Warning => "warning",
}

impl fmt::Display for ChartEventName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Chart-level events emitted on `chart.on(...)`: one variant per
/// [`ChartEventName`], carrying that event's payload.
#[derive(Clone, Debug)]
pub enum ChartEvent {
    Ready,
    /// The chart's market switched IN PLACE via `setMarket`: symbol, provider,
    /// timeframe, or offline data changed (a depth-only reload does not fire).
    /// Fires after the new market's history is painted and every consumer
    /// restarted. `prev` carries the previous identity so hosts can re-key
    /// per-symbol state (swap user-drawing documents between symbols).
    MarketChanged {
        symbol: String,
        timeframe: String,
        prev: MarketIdentity,
    },
    /// A bar load began with nothing painted: the FIRST load (fires during
    /// construction: subscribers attached later see only its `load:end`), or an
    /// identity switch (symbol/provider/timeframe), which blanks the old series
    /// in the same breath. Fires before the first fetch: plugins, extensions and
    /// custom indicators hide or reset their own visuals here. Exactly one
    /// `load:end` follows. A depth-only reload keeps the chart painted and fires
    /// neither.
    LoadStart {
        symbol: String,
        timeframe: String,
        first_load: bool,
    },
    /// The load ended: its first bars painted (`bars` > 0: on deep histories the
    /// quick preview, before the full depth), or it ended with none (`bars` = 0:
    /// a failed fetch, an empty market, or a parked symbol nothing serves).
    /// Counterpart of `load:start`; plugins restore or rebuild their visuals here.
    LoadEnd {
        symbol: String,
        timeframe: String,
        bars: usize,
    },
    IndicatorAdded {
        id: String,
    },
    IndicatorRemoved {
        id: String,
    },
    /// An indicator's stored input/prop VALUES changed (settings dialog,
    /// `setInputs`): what state persistence listens to.
    IndicatorInputs {
        id: String,
    },
    /// `error` is the failure's message.
    IndicatorError {
        id: String,
        error: String,
    },
    /// No registered provider can serve the chart symbol: the load is PARKED, not
    /// failed; it resumes by itself if a capable provider registers later.
    DataUnresolved {
        symbol: String,
        providers: Vec<String>,
    },
    /// The source refused the chart's own series for good (a provider returned a
    /// `DataUnavailableError`: a monthly series asked for hourly bars). The load
    /// has ended (`load:end` with what was painted, `history:complete` with
    /// `Aborted`) and nothing streams or polls for this market. `symbol` /
    /// `timeframe` are the chart's market as set; `finest` / `default` are the
    /// source's hints, in its own timeframe notation. The chart never changes its
    /// timeframe by itself: falling back is the host's call. A secondary fetch
    /// (`request.security`) never fires it: its future errors instead.
    DataUnavailable {
        symbol: String,
        timeframe: String,
        reason: DataUnavailableReason,
        finest: Option<String>,
        default: Option<String>,
    },
    /// The facts the chart applies about its symbol changed
    /// (`chart.symbolFacts()`): they landed for the symbol on screen (`facts`), or
    /// a symbol switch reset them to unknown (`None`) until the new symbol's land.
    /// They decide which price styles can draw the symbol: by the time it fires
    /// the chart already draws its fallback for a style they rule out
    /// (`chart.renderer.get('shownPriceStyle')`), so a host's style picker
    /// re-reads here which styles to grey.
    SymbolFacts {
        symbol: String,
        facts: Option<SymbolFacts>,
    },
    /// An indicator was moved/merged to another pane (`chart.panes` / legend /
    /// object tree).
    IndicatorMoved {
        id: String,
        pane_id: String,
    },
    /// An indicator was shown/hidden (legend eye, `handle.setVisible`, or object tree).
    IndicatorVisibility {
        id: String,
        visible: bool,
    },
    /// A pane's layout changed: order, collapse/maximize, creation or removal.
    PaneChanged,
    /// The app theme changed: `chart.setTheme(...)` or the in-chart settings
    /// dialog's Canvas, Theme row. The payload is the RESOLVED theme; host chrome
    /// around the chart re-skins from it. Not fired for plot-only cosmetic edits
    /// (`layout.background` through the config), which deliberately leave the app
    /// theme alone.
    ThemeChanged(VelaTheme),
    /// The chart's localization changed through `chart.setLocale(...)`, after the
    /// chart has re-rendered its text and numbers. Carries the resolved locale and
    /// chrome direction so host chrome around the chart can follow.
    LocaleChanged(LocaleChangedEvent),
    /// The base price style is switching `from` to `to` (candles, line, area, a
    /// chart-type id, ...). Fires synchronously BEFORE the chart repaints in the
    /// new style, whatever the path: the topbar style menu, `ctx.setPriceStyle`,
    /// `chart.renderer.set('priceStyle', ...)`, the settings dialog, `applyConfig`
    /// and config templates, or a state restore. Never fires for the style the
    /// chart was created with, nor for a set to the style already shown. While it
    /// runs the chart still shows `from`, so a listener can capture the outgoing
    /// frame to animate the switch. Listeners must not change the price style
    /// again from inside it.
    PriceStyleChange {
        from: String,
        to: String,
    },
    /// A study pane was reordered one slot (`dir`): carries enough to invert for
    /// undo/redo.
    PaneMoved {
        pane_id: String,
        dir: PaneMoveDir,
    },
    /// A user drawing was created (interactively or via `chart.drawings.add`).
    DrawingCreated {
        id: String,
    },
    /// An interactive placement is in progress: the ghost's current shape after
    /// each anchor click / cursor move, `None` when placement ends (finalized or
    /// cancelled). Transient: nothing is in the store yet. Multi-chart hosts
    /// mirror it on linked charts via `drawings.setExternalGhost`.
    DrawingDraft {
        doc: Option<SerializedDrawing>,
    },
    /// A user drawing's anchors/style/text changed.
    DrawingEdited {
        id: String,
    },
    /// Selection changed. `ids` is every selected drawing in selection order; `id`
    /// is the primary (`ids[0]`, the one a settings popup edits), `None` when
    /// nothing is selected.
    DrawingSelected {
        id: Option<String>,
        ids: Vec<String>,
    },
    /// The favorite-tool set changed (star toggles or a bulk restore).
    DrawingFavorites {
        favorites: Vec<String>,
    },
    /// A tool's remembered settings changed: the ones its next drawing starts from
    /// (the last drawing of that type the user styled, a bulk restore, or a
    /// reset). Read them back with `drawings.toolDefaults()`.
    DrawingDefaults {
        type_key: DrawingTypeKey,
    },
    /// A tool's saved looks changed (saved, deleted, or a bulk restore): read them
    /// back with `drawings.toolTemplates()`.
    DrawingTemplates {
        type_key: DrawingTypeKey,
    },
    /// The armed drawing tool changed: toolbar click, one-shot tool finishing
    /// (back to the pointer, `None`), or a programmatic `drawings.setTool`.
    DrawingTool {
        type_key: Option<DrawingTypeKey>,
    },
    /// The magnet snap mode changed (in-chart toolbar or `drawings.setSnapMode`).
    DrawingSnap {
        mode: SnapMode,
    },
    /// Stay-in-drawing-mode changed (in-chart toolbar or `drawings.setStayMode`):
    /// when on, finishing a drawing leaves the tool armed.
    DrawingStay {
        on: bool,
    },
    /// The renderer-local mode changed: measure ruler, eraser, or none (`None`),
    /// including the mutual-exclusion exits (arming a tool leaves measure/eraser).
    DrawingMode {
        mode: Option<DrawingMode>,
    },
    /// A user drawing was removed.
    DrawingRemoved {
        id: String,
    },
    /// The user requested a drawing's settings popup.
    DrawingSettings {
        id: String,
    },
    /// A timeline-mark glyph was clicked: `ids` lists every mark under it (a
    /// cluster). Fires before the popup opens; a mark without content opens none,
    /// so this is the host's hook.
    MarkClick(MarkClickEvent),
    /// A SCRIPT computed: the first run over the history, a live tick, a new bar,
    /// an input edit, a viewport move, a market switch. The payload carries the
    /// run itself (title, cause, the plots/variables/broker state at the computed
    /// bar), so a listener reads it directly instead of resolving a handle and
    /// pulling a snapshot. Throttled to about 1/s per indicator while streaming,
    /// and only emitted for engines that expose an execution context. Native
    /// (core-computed) indicators never fire it: they run no script.
    ScriptRun(ScriptRun),
    /// An indicator's execution context advanced (run finished, or throttled
    /// during streaming): re-pull `handle.context()` if you consume it. Prefer
    /// `script:run`, which delivers the data rather than a signal to go fetch it.
    ContextChanged {
        id: String,
    },
    /// A live tick: the forming bar was updated or a new bar appended (a bar
    /// revealed by replay included).
    Bar(Ohlcv),
    /// The visible time range moved (pan/zoom/fit: fires per applied change, NOT
    /// debounced; the engine re-run debounce is separate). Epoch-ms. The seam
    /// viewport-sync links between charts build on.
    ViewportChanged {
        from: Millis,
        to: Millis,
    },
    /// A deep-history backfill chunk landed (`loaded` of `target` bars are on the chart).
    HistoryProgress {
        loaded: usize,
        target: usize,
    },
    /// The history load finished. Fires exactly once, including for small/offline
    /// charts (immediately after their single load).
    HistoryComplete {
        reason: HistoryReason,
        oldest_time: Millis,
        bars_loaded: usize,
    },
    /// Replay mode began (or seeked): the chart shows history up to `cursor_time`,
    /// `remaining` bars are hidden.
    ReplayStart {
        cursor_time: Millis,
        remaining: usize,
    },
    /// One hidden bar was revealed (it also fires `bar`, like a live bar). A bar
    /// played tick by tick steps once, when its last tick settles it.
    ReplayStep {
        cursor_time: Millis,
        remaining: usize,
    },
    /// An intrabar update of the bar forming at `cursor_time`
    /// (`chart.replay.setTicks`): tick `index` of `count` is applied (batched
    /// updates report the last one).
    ReplayTick {
        cursor_time: Millis,
        index: usize,
        count: usize,
    },
    /// Timed reveal started, or its pace changed.
    ReplayPlay {
        interval_ms: f64,
    },
    /// Timed reveal paused: `pause()`, or a seek (`start()` while playing).
    ReplayPause,
    /// Replay mode ended. The full history is back and live updates resumed.
    ReplayEnd {
        reason: ReplayEndReason,
    },
    /// An indicator's script raised an alert.
    Alert(AlertEvent),
    Warning(EngineWarning),
}

impl ChartEvent {
    /// The name this event is emitted under.
    pub fn name(&self) -> ChartEventName {
        use ChartEventName as N;
        match self {
            Self::Ready => N::Ready,
            Self::MarketChanged { .. } => N::MarketChanged,
            Self::LoadStart { .. } => N::LoadStart,
            Self::LoadEnd { .. } => N::LoadEnd,
            Self::IndicatorAdded { .. } => N::IndicatorAdded,
            Self::IndicatorRemoved { .. } => N::IndicatorRemoved,
            Self::IndicatorInputs { .. } => N::IndicatorInputs,
            Self::IndicatorError { .. } => N::IndicatorError,
            Self::DataUnresolved { .. } => N::DataUnresolved,
            Self::DataUnavailable { .. } => N::DataUnavailable,
            Self::SymbolFacts { .. } => N::SymbolFacts,
            Self::IndicatorMoved { .. } => N::IndicatorMoved,
            Self::IndicatorVisibility { .. } => N::IndicatorVisibility,
            Self::PaneChanged => N::PaneChanged,
            Self::ThemeChanged(_) => N::ThemeChanged,
            Self::LocaleChanged(_) => N::LocaleChanged,
            Self::PriceStyleChange { .. } => N::PriceStyleChange,
            Self::PaneMoved { .. } => N::PaneMoved,
            Self::DrawingCreated { .. } => N::DrawingCreated,
            Self::DrawingDraft { .. } => N::DrawingDraft,
            Self::DrawingEdited { .. } => N::DrawingEdited,
            Self::DrawingSelected { .. } => N::DrawingSelected,
            Self::DrawingFavorites { .. } => N::DrawingFavorites,
            Self::DrawingDefaults { .. } => N::DrawingDefaults,
            Self::DrawingTemplates { .. } => N::DrawingTemplates,
            Self::DrawingTool { .. } => N::DrawingTool,
            Self::DrawingSnap { .. } => N::DrawingSnap,
            Self::DrawingStay { .. } => N::DrawingStay,
            Self::DrawingMode { .. } => N::DrawingMode,
            Self::DrawingRemoved { .. } => N::DrawingRemoved,
            Self::DrawingSettings { .. } => N::DrawingSettings,
            Self::MarkClick(_) => N::MarkClick,
            Self::ScriptRun(_) => N::ScriptRun,
            Self::ContextChanged { .. } => N::ContextChanged,
            Self::Bar(_) => N::Bar,
            Self::ViewportChanged { .. } => N::ViewportChanged,
            Self::HistoryProgress { .. } => N::HistoryProgress,
            Self::HistoryComplete { .. } => N::HistoryComplete,
            Self::ReplayStart { .. } => N::ReplayStart,
            Self::ReplayStep { .. } => N::ReplayStep,
            Self::ReplayTick { .. } => N::ReplayTick,
            Self::ReplayPlay { .. } => N::ReplayPlay,
            Self::ReplayPause => N::ReplayPause,
            Self::ReplayEnd { .. } => N::ReplayEnd,
            Self::Alert(_) => N::Alert,
            Self::Warning(_) => N::Warning,
        }
    }
}

/// The event bus a chart owns.
pub type ChartEventBus = super::TypedEventBus<ChartEventName, ChartEvent>;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    /// The original's `VelaEventMap` keys, in declaration order.
    const TS_EVENT_NAMES: [&str; 46] = [
        "ready",
        "market:changed",
        "load:start",
        "load:end",
        "indicator:added",
        "indicator:removed",
        "indicator:inputs",
        "indicator:error",
        "data:unresolved",
        "data:unavailable",
        "symbol:facts",
        "indicator:moved",
        "indicator:visibility",
        "pane:changed",
        "theme:changed",
        "locale:changed",
        "priceStyle:change",
        "pane:moved",
        "drawing:created",
        "drawing:draft",
        "drawing:edited",
        "drawing:selected",
        "drawing:favorites",
        "drawing:defaults",
        "drawing:templates",
        "drawing:tool",
        "drawing:snap",
        "drawing:stay",
        "drawing:mode",
        "drawing:removed",
        "drawing:settings",
        "mark:click",
        "script:run",
        "context:changed",
        "bar",
        "viewport:changed",
        "history:progress",
        "history:complete",
        "replay:start",
        "replay:step",
        "replay:tick",
        "replay:play",
        "replay:pause",
        "replay:end",
        "alert",
        "warning",
    ];

    fn script_run() -> ScriptRun {
        ScriptRun {
            id: "i1".into(),
            title: "SMA".into(),
            kind: ScriptKind::Indicator,
            cause: ScriptRunCause::History,
            first: true,
            bar: 10,
            time: 1,
            forming: false,
            complete: true,
            plots: BTreeMap::new(),
            vars: BTreeMap::new(),
            strategy: None,
            warnings: Vec::new(),
            trades_loader: None,
            series_loader: None,
        }
    }

    /// One event per name, in declaration order.
    fn one_of_each() -> Vec<ChartEvent> {
        let id = || "x".to_string();
        let bar = Ohlcv::new(1, 1.0, 2.0, 0.5, 1.5, None);
        vec![
            ChartEvent::Ready,
            ChartEvent::MarketChanged {
                symbol: "B".into(),
                timeframe: "60".into(),
                prev: MarketIdentity {
                    symbol: "A".into(),
                    timeframe: "D".into(),
                },
            },
            ChartEvent::LoadStart {
                symbol: id(),
                timeframe: id(),
                first_load: true,
            },
            ChartEvent::LoadEnd {
                symbol: id(),
                timeframe: id(),
                bars: 0,
            },
            ChartEvent::IndicatorAdded { id: id() },
            ChartEvent::IndicatorRemoved { id: id() },
            ChartEvent::IndicatorInputs { id: id() },
            ChartEvent::IndicatorError {
                id: id(),
                error: "boom".into(),
            },
            ChartEvent::DataUnresolved {
                symbol: id(),
                providers: vec![],
            },
            ChartEvent::DataUnavailable {
                symbol: id(),
                timeframe: id(),
                reason: DataUnavailableReason::Timeframe,
                finest: None,
                default: Some("D".into()),
            },
            ChartEvent::SymbolFacts {
                symbol: id(),
                facts: None,
            },
            ChartEvent::IndicatorMoved {
                id: id(),
                pane_id: id(),
            },
            ChartEvent::IndicatorVisibility {
                id: id(),
                visible: true,
            },
            ChartEvent::PaneChanged,
            ChartEvent::ThemeChanged(json!({"background": "#000"})),
            ChartEvent::LocaleChanged(LocaleChangedEvent {
                locale: "en".into(),
                direction: crate::intl::TextDirection::Ltr,
            }),
            ChartEvent::PriceStyleChange {
                from: "candles".into(),
                to: "line".into(),
            },
            ChartEvent::PaneMoved {
                pane_id: id(),
                dir: PaneMoveDir::Up,
            },
            ChartEvent::DrawingCreated { id: id() },
            ChartEvent::DrawingDraft { doc: None },
            ChartEvent::DrawingEdited { id: id() },
            ChartEvent::DrawingSelected {
                id: None,
                ids: vec![],
            },
            ChartEvent::DrawingFavorites { favorites: vec![] },
            ChartEvent::DrawingDefaults { type_key: id() },
            ChartEvent::DrawingTemplates { type_key: id() },
            ChartEvent::DrawingTool { type_key: None },
            ChartEvent::DrawingSnap {
                mode: SnapMode::Weak,
            },
            ChartEvent::DrawingStay { on: true },
            ChartEvent::DrawingMode { mode: None },
            ChartEvent::DrawingRemoved { id: id() },
            ChartEvent::DrawingSettings { id: id() },
            ChartEvent::MarkClick(MarkClickEvent {
                id: id(),
                ids: vec![id()],
                time: 1,
                group: None,
            }),
            ChartEvent::ScriptRun(script_run()),
            ChartEvent::ContextChanged { id: id() },
            ChartEvent::Bar(bar),
            ChartEvent::ViewportChanged { from: 1, to: 2 },
            ChartEvent::HistoryProgress {
                loaded: 1,
                target: 2,
            },
            ChartEvent::HistoryComplete {
                reason: HistoryReason::Genesis,
                oldest_time: 1,
                bars_loaded: 2,
            },
            ChartEvent::ReplayStart {
                cursor_time: 1,
                remaining: 2,
            },
            ChartEvent::ReplayStep {
                cursor_time: 1,
                remaining: 2,
            },
            ChartEvent::ReplayTick {
                cursor_time: 1,
                index: 0,
                count: 3,
            },
            ChartEvent::ReplayPlay { interval_ms: 250.0 },
            ChartEvent::ReplayPause,
            ChartEvent::ReplayEnd {
                reason: ReplayEndReason::Market,
            },
            ChartEvent::Alert(AlertEvent {
                alert: EngineAlert {
                    id: id(),
                    message: id(),
                    title: None,
                    time: 1,
                    bar_index: 2,
                    freq: None,
                },
                indicator: Some("SMA".into()),
            }),
            ChartEvent::Warning(EngineWarning {
                message: id(),
                method: None,
                bar: 1,
            }),
        ]
    }

    #[test]
    fn event_names_match_the_original_exactly_and_in_order() {
        let ours: Vec<&str> = ChartEventName::ALL.iter().map(|n| n.as_str()).collect();
        assert_eq!(ours, TS_EVENT_NAMES);
        for name in TS_EVENT_NAMES {
            let parsed = ChartEventName::from_name(name).expect(name);
            assert_eq!(parsed.as_str(), name);
            assert_eq!(parsed.to_string(), name);
        }
        assert_eq!(ChartEventName::from_name("nope"), None);
        assert_eq!(ChartEventName::from_name("Bar"), None);
    }

    #[test]
    fn every_event_reports_its_name() {
        let events = one_of_each();
        assert_eq!(events.len(), TS_EVENT_NAMES.len());
        for (event, expected) in events.iter().zip(TS_EVENT_NAMES) {
            assert_eq!(event.name().as_str(), expected);
        }
    }

    #[test]
    fn the_enums_drive_a_typed_event_bus() {
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let bus = ChartEventBus::new();
        let sink = seen.clone();
        bus.on(ChartEventName::IndicatorAdded, move |event| {
            if let ChartEvent::IndicatorAdded { id } = event {
                sink.borrow_mut().push(id.clone());
            }
        });
        let sink = seen.clone();
        bus.on(ChartEventName::Bar, move |event| {
            if let ChartEvent::Bar(bar) = event {
                sink.borrow_mut().push(format!("bar {}", bar.time));
            }
        });
        for event in [
            ChartEvent::IndicatorAdded { id: "sma".into() },
            ChartEvent::Bar(Ohlcv::new(5, 1.0, 1.0, 1.0, 1.0, None)),
            ChartEvent::Ready,
        ] {
            bus.emit(&event.name(), &event);
        }
        assert_eq!(*seen.borrow(), ["sma", "bar 5"]);
        assert!(bus.has_listeners(&ChartEventName::Bar));
        assert!(!bus.has_listeners(&ChartEventName::Ready));
    }

    #[test]
    fn script_run_loads_trades_and_series_lazily() {
        let platform = ManualPlatform::new(0.0);
        let run = script_run();
        assert!(platform.block_on(run.trades()).is_empty());
        assert!(platform.block_on(run.series("sma")).is_empty());

        let loaded = ScriptRun {
            series_loader: Some(Rc::new(|key| {
                let key = key.to_owned();
                Box::pin(async move {
                    if key == "sma" {
                        vec![PlotSeriesPoint {
                            time: 1,
                            value: Some(2.0),
                        }]
                    } else {
                        Vec::new()
                    }
                })
            })),
            ..script_run()
        };
        assert_eq!(platform.block_on(loaded.series("sma")).len(), 1);
        assert!(platform.block_on(loaded.series("other")).is_empty());
    }

    #[test]
    fn payload_enums_use_the_original_spellings() {
        assert_eq!(
            serde_json::to_value(HistoryReason::Aborted).unwrap(),
            json!("aborted")
        );
        for (reason, name) in [
            (ReplayEndReason::Stopped, "stopped"),
            (ReplayEndReason::Finished, "finished"),
            (ReplayEndReason::Market, "market"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<ReplayEndReason>(json!(name)).unwrap(),
                reason
            );
        }
        for (cause, name) in [
            (ScriptRunCause::History, "history"),
            (ScriptRunCause::Tick, "tick"),
            (ScriptRunCause::Bar, "bar"),
            (ScriptRunCause::Inputs, "inputs"),
            (ScriptRunCause::Code, "code"),
            (ScriptRunCause::Viewport, "viewport"),
            (ScriptRunCause::Market, "market"),
        ] {
            assert_eq!(serde_json::to_value(cause).unwrap(), json!(name));
        }
        assert_eq!(
            serde_json::to_value(ScriptKind::Strategy).unwrap(),
            json!("strategy")
        );
    }

    #[test]
    fn plain_payloads_round_trip() {
        let identity = MarketIdentity {
            symbol: "BTCUSDT".into(),
            timeframe: "60".into(),
        };
        let wire = serde_json::to_value(&identity).unwrap();
        assert_eq!(wire, json!({"symbol": "BTCUSDT", "timeframe": "60"}));
        assert_eq!(
            serde_json::from_value::<MarketIdentity>(wire).unwrap(),
            identity
        );

        let alert = AlertEvent {
            alert: EngineAlert {
                id: "a".into(),
                message: "m".into(),
                title: Some("t".into()),
                time: 5,
                bar_index: 6,
                freq: None,
            },
            indicator: Some("SMA".into()),
        };
        let wire = serde_json::to_value(&alert).unwrap();
        assert_eq!(wire["barIndex"], json!(6));
        assert_eq!(wire["indicator"], json!("SMA"));
        assert_eq!(serde_json::from_value::<AlertEvent>(wire).unwrap(), alert);

        let point = PlotSeriesPoint {
            time: 3,
            value: None,
        };
        assert_eq!(
            serde_json::from_value::<PlotSeriesPoint>(serde_json::to_value(point).unwrap())
                .unwrap(),
            point
        );
    }
}
