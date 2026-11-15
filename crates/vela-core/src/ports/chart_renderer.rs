//! The rendering-backend port: [`ChartRenderer`] (the original `IChartRenderer`),
//! the renderer-facing drawing port ([`DrawingsRendererPort`], the original
//! `IDrawingsRendererPort` of `drawings/port.ts`) and every type either declares.
//!
//! No backend type crosses this boundary: that is what makes the renderer
//! swappable.
//!
//! # Conventions
//!
//! - Every method takes `&self`: a renderer owns its interior state (`Cell` /
//!   `RefCell`) and is shared as `Rc<dyn ChartRenderer>`. Callers must not call
//!   into a renderer from inside one of its callbacks while the renderer is
//!   mid-update; the renderer, for its part, must not hold a borrow of its own
//!   state while it invokes a callback.
//! - An optional TypeScript method (`foo?(...)`) is a trait method with a default
//!   body that reports "not supported": it does nothing, or returns `None` /
//!   `false` / an empty collection. Where the original caller tests for presence
//!   (`if (renderer.foo)`, `typeof renderer.foo === 'function'`) there is a
//!   `supports_foo(&self) -> bool`, defaulting to `false`, that an implementation
//!   overriding `foo` must also override. An optional `onFoo` subscription returns
//!   `Option<Unsubscribe>`: `None` means the renderer has no such event (the
//!   original's `renderer.onFoo?.(cb) ?? noop`).
//! - Callbacks the renderer invokes are `Box<dyn FnMut(&T)>` ([`Listener`]); the
//!   returned [`Unsubscribe`] detaches them.
//!
//! # What is opaque or left out
//!
//! - `mount(container: HTMLElement, theme)`: a DOM node has no place in core. The
//!   [`ChartRenderer::mount`] method takes the theme only; a DOM-owning renderer
//!   is handed its container at construction by its host crate.
//! - `screenshotCanvas(): HTMLCanvasElement | null` is left out (DOM). The
//!   string form [`ChartRenderer::screenshot`] stays.
//! - [`ToolbarDefinition`] (`drawings/toolbar.ts`, plain data: groups, tools,
//!   inline SVG icons) is not ported: it is an opaque [`serde_json::Value`] that
//!   only passes through to [`DrawingsRendererPort::set_toolbar`].
//! - [`VelaTheme`] (`options.ts`, a bag of colors plus UI-kit settings) is an
//!   opaque [`serde_json::Value`]; [`ThemeId`] and [`PriceStyle`] are plain
//!   `String`s, because both are open sets (`'dark' | 'light' | (string & {})`).
//! - [`ChartRenderer::apply_feature`] / [`ChartRenderer::read_feature`] /
//!   [`ChartRenderer::set_native_data`] / [`ChartRenderer::get_config`] /
//!   [`ChartRenderer::apply_config`] carry the original's `unknown` as
//!   `serde_json::Value`. "Undefined" (an unsupported feature) is `None`.
//! - [`SettingsSection::rows`] (the host-app callback rows of the settings dialog)
//!   is `Vec<serde_json::Value>`: a DOM-and-closure shape the core only forwards.
//! - [`WallClock`] (`util/wall-clock.ts`) is ported as the trait only; the
//!   `SecondClock` implementation needs timers and belongs with the platform.
//! - [`SymbolPickerFn`] is ported here (the model layer did not carry it).
//! - `DrawingTypeKey` is a `String` (the registry's open key set), as in
//!   [`SerializedDrawing::type_key`].
//! - [`PaneAction`], [`DrawingIntent`] and [`InputChangeEvent`] are not serde
//!   types (they hold types that are not); the plain payload structs are.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::drawings::style::DrawingStyle;
use crate::drawings::{
    DrawingSeriesGateway, DrawingToolDefaults, DrawingToolTemplate, SerializedDrawing, SnapMode,
};
use crate::i18n::{MessageLike, Translator};
pub use crate::indicators::{IndicatorStatus, VisibleRange};
use crate::intl::TextDirection;
use crate::marks::{MarkClickEvent, MarkGroup, TimelineMark};
use crate::model::{IndicatorModel, InputValue, Inputs, Millis, Ohlcv, Pane, PaneKind, ScenePatch};
use crate::ports::market_data::SymbolFacts;
use crate::runtime::Unsubscribe;

/// A callback the renderer invokes with a borrowed payload.
pub type Listener<T> = Box<dyn FnMut(&T)>;
/// Callback of [`ChartRenderer::on_move_indicator`]: `(indicator id, target)`.
pub type MoveIndicatorListener = Box<dyn FnMut(&str, &MoveTarget)>;
/// Callback of [`ChartRenderer::on_price_style_will_change`]: `(from, to)`.
pub type PriceStyleWillChangeListener = Box<dyn FnMut(&PriceStyle, &PriceStyle)>;
/// Callback of [`ChartRenderer::on_toggle_indicator_visible`]: `(id, visible)`.
pub type ToggleVisibleListener = Box<dyn FnMut(&str, bool)>;
/// Callback of [`ChartRenderer::on_chart_type_settings_change`]: `(type id, values)`.
pub type ChartTypeSettingsListener = Box<dyn FnMut(&str, &serde_json::Map<String, Value>)>;

/// The app theme the renderer paints with (`VelaTheme`): opaque, see the module doc.
pub type VelaTheme = Value;
/// A built-in theme's name (`dark`, `light`) or the id of a registered theme.
pub type ThemeId = String;
/// The base price style (`candles`, `line`, a chart-type id, ...): an open set.
pub type PriceStyle = String;
/// The inert toolbar definition a renderer paints: opaque, see the module doc.
pub type ToolbarDefinition = Value;
/// An interactive drawing type key (`trendline`, `fibretracement`, ...).
pub type DrawingTypeKey = String;

/// Opens the host's own ticker-selection UI for an `input.symbol` control:
/// called with the current symbol and a callback to report the pick.
pub type SymbolPickerFn = Rc<dyn Fn(&str, Box<dyn FnOnce(String)>)>;

// ── wall clock ──

/// A once-per-second wall-clock pulse. Every subscriber receives the SAME `now`
/// for a given second, so chrome that displays time (a clock, a countdown to
/// bar close) is driven from one source and never disagrees about the second.
pub trait WallClock {
    /// `cb` runs shortly after every wall-clock second boundary with that
    /// instant (epoch ms).
    fn on_tick(&self, cb: Box<dyn FnMut(f64)>) -> Unsubscribe;
}

// ── capabilities ──

/// How a visual is supported: natively by the backend, through a custom
/// primitive, or not at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Support {
    Native,
    Primitive,
    Unsupported,
}

/// How per-bar candle recoloring is supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarColorSupport {
    Native,
    Approximated,
    Unsupported,
}

/// What a rendering backend supports: drives graceful degradation and warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererCapabilities {
    pub panes: bool,
    /// Full pane management: moving/merging an indicator between panes (with its
    /// own scale column), reordering panes, and pane collapse/maximize. A
    /// renderer without it keeps one-pane-per-indicator behavior; `chart.panes`
    /// mutations warn and no-op.
    pub pane_management: bool,
    pub fills: Support,
    pub bgcolor: Support,
    pub hline: Support,
    pub markers: bool,
    pub barcolor: BarColorSupport,
    pub per_point_color: bool,
    /// Pine drawing objects (line/box/label/polyline/linefill) via custom primitives.
    pub drawings: bool,
    /// Interactive USER drawing tools (toolbar + hit-test + handles). Distinct
    /// from `drawings`.
    pub user_drawings: bool,
    /// Whether user drawings share ONE draw-order space with the pane's series:
    /// a drawing's `z_index` then places it anywhere in the stack (over
    /// everything, under the candles, or between two indicators). Absent/false:
    /// every drawing paints in front, `z_index` orders only the drawings among
    /// themselves, and a host UI should not offer depth slots.
    #[serde(default)]
    pub drawing_depth: bool,
    /// Pine `table.new` dashboards via a DOM overlay.
    pub tables: bool,
    /// Strategy trade markers (`IndicatorModel::trades`): order-fill arrows,
    /// labels and price ticks on the price pane, plus the `tradeMarkers` display
    /// feature. Absent/false: the channel is carried through mounts and patches
    /// but never painted.
    #[serde(default)]
    pub trades: bool,
    /// Timeline marks (`chart.marks`): host events pinned to a bar, painted as
    /// glyphs on a lane above the time axis with a detail popup on click.
    /// Absent/false: the model still fills but nothing paints.
    #[serde(default)]
    pub timeline_marks: bool,
    /// Whether the renderer provides the in-chart inputs/settings UI.
    #[serde(rename = "inputsUI")]
    pub inputs_ui: bool,
}

/// The language and direction attributes of a renderer's root element
/// (`None` = unset: leave the attribute alone, the host page decides).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererRootAttributes {
    pub lang: Option<String>,
    pub dir: Option<TextDirection>,
}

/// Opaque handle to a mounted indicator, returned by
/// [`ChartRenderer::mount_indicator`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IndicatorRenderHandle {
    pub id: String,
}

// ── legend ──

/// One host-contributed legend-row action, as the renderer consumes it: pure
/// data plus a thunk. The shell resolves the plugin descriptor (its `when` gate,
/// the context, the indicator info) BEFORE it reaches the renderer, so the
/// renderer stays ignorant of the plugin layer: it paints an icon button and
/// calls `run` on click.
#[derive(Clone)]
pub struct LegendActionView {
    pub id: String,
    /// Icon id in the core icon registry (`registerIcon`).
    pub icon: String,
    pub tooltip: String,
    pub run: Rc<dyn Fn()>,
}

/// One block of a legend callout's deployed panel: plain text, or an action button.
#[derive(Clone)]
pub enum LegendCalloutPanelItem {
    Text {
        text: String,
    },
    Button {
        label: String,
        /// Emphasized (selection-colored) button: the panel's main action.
        primary: bool,
        /// Close the panel after `run`. `None` means the default, `true`.
        close: Option<bool>,
        run: Rc<dyn Fn()>,
    },
}

impl LegendCalloutPanelItem {
    /// Whether pressing this block closes the panel (buttons close by default).
    pub fn closes_panel(&self) -> bool {
        match self {
            Self::Button { close, .. } => close.unwrap_or(true),
            Self::Text { .. } => false,
        }
    }
}

/// The panel a clickable legend callout deploys: an optional heading over
/// ordered blocks.
#[derive(Clone, Default)]
pub struct LegendCalloutPanel {
    pub title: Option<String>,
    pub items: Vec<LegendCalloutPanelItem>,
}

/// One host-contributed legend CALLOUT, as the renderer consumes it: a small
/// tinted bubble with a centered icon, visible beside the indicator's legend
/// title while the row is idle (hidden while its hover/selection controls are
/// out). When `content` is present the bubble is clickable and deploys that
/// panel, below the bubble, flipping above when the bottom screen edge is too
/// close. Pure data plus thunks, same rule as [`LegendActionView`].
#[derive(Clone)]
pub struct LegendCalloutView {
    pub id: String,
    /// Icon id in the core icon registry (`registerIcon`).
    pub icon: String,
    /// Bubble fill: any CSS color.
    pub background: String,
    /// Icon ink (default: the legend row's text color).
    pub color: Option<String>,
    pub tooltip: String,
    /// Deployed panel: its presence makes the bubble clickable.
    pub content: Option<LegendCalloutPanel>,
}

/// Supplies the legend actions of one indicator row, lazily per row.
pub type LegendActionProvider = Rc<dyn Fn(&str) -> Vec<LegendActionView>>;
/// Supplies the legend callouts of one indicator row, lazily per row.
pub type LegendCalloutProvider = Rc<dyn Fn(&str) -> Vec<LegendCalloutView>>;

// ── pointer and readout events ──

/// OHLCV of the price bar under the crosshair (the "data window" source).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrosshairOhlc {
    pub time: Millis,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
}

/// The crosshair moved.
#[derive(Clone, Debug, PartialEq)]
pub struct CrosshairEvent {
    pub time: Option<Millis>,
    /// Value at the cursor on ITS pane's scale: a price on the price pane, an
    /// indicator value on a study pane (which one: see `pane_kind`).
    pub price: Option<f64>,
    /// The kind of pane the cursor (and thus `price`) is on: how a consumer tells
    /// a real price from a study-pane value (crosshair sync only mirrors the
    /// horizontal level from the price pane). A renderer that does not track
    /// panes leaves it `None`.
    pub pane_kind: Option<PaneKind>,
    /// The id of the pane under the cursor (the id `chart.panes.list()` reports);
    /// `None` off the plot or when the renderer does not track panes.
    pub pane_id: Option<String>,
    /// Value at the crosshair per series, keyed by stable series id, in the
    /// renderer's order.
    pub values: Vec<(String, f64)>,
    /// The hovered price bar's OHLCV (`None` when the cursor is off any bar).
    pub ohlc: Option<CrosshairOhlc>,
}

impl CrosshairEvent {
    /// The value of series `id` at the crosshair.
    pub fn value(&self, id: &str) -> Option<f64> {
        self.values
            .iter()
            .find_map(|(key, value)| (key == id).then_some(*value))
    }
}

/// The plot was clicked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClickEvent {
    pub time: Option<Millis>,
    /// Value at the click on its pane's scale, as [`CrosshairEvent::price`].
    pub price: Option<f64>,
    /// The kind of pane clicked; `None` off the plot or when untracked.
    pub pane_kind: Option<PaneKind>,
    /// The id of the pane clicked; `None` off the plot or when untracked.
    pub pane_id: Option<String>,
}

/// Which axis strip a long-press hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Price,
    Time,
}

/// A touch long-press on an axis strip: the mobile substitute for a right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxisLongPressEvent {
    pub axis: Axis,
    /// Plot-local pixel of the press (`y` maps a multi-pane price scale to its pane).
    pub x: f64,
    pub y: f64,
}

/// One indicator plot's readout line in the data window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataWindowRow {
    pub label: String,
    pub value: String,
    pub color: String,
}

/// The OHLCV block of a data-window readout, formatted on the price pane's scale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataWindowOhlc {
    pub o: String,
    pub h: String,
    pub l: String,
    pub c: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vol: Option<String>,
    /// Close >= open: tint the values with the up color, else the down color.
    pub up: bool,
}

/// One indicator's readout: its title plus a row per plot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataWindowGroup {
    pub name: String,
    pub rows: Vec<DataWindowRow>,
}

/// A data-window snapshot: the bar's timestamp split into date and time, its
/// OHLCV, and one group per indicator. Every value arrives pre-formatted on the
/// scale of the pane it belongs to, so a host panel lays the readout out without
/// doing any numeric formatting itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataWindowReadout {
    /// Bar date, pre-formatted (e.g. `2026-07-03`); empty when there is no bar.
    pub date: String,
    /// Bar time of day, pre-formatted `HH:MM`; empty when there is no bar.
    pub time: String,
    pub ohlc: Option<DataWindowOhlc>,
    pub groups: Vec<DataWindowGroup>,
}

// ── input edits, viewport, panes ──

/// What an [`InputChangeEvent`] edited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputChangeKind {
    /// A script input (the default).
    #[default]
    Input,
    /// A declaration prop (the "Properties" tab).
    Prop,
}

/// Raised by the renderer when the user edits an input in the settings dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct InputChangeEvent {
    pub indicator_id: String,
    pub key: String,
    pub value: InputValue,
    /// What was edited. `None` means a script input, as [`InputChangeKind::Input`].
    pub kind: Option<InputChangeKind>,
}

impl InputChangeEvent {
    /// The edit's kind with the default applied.
    pub fn kind(&self) -> InputChangeKind {
        self.kind.unwrap_or_default()
    }
}

/// How [`ChartRenderer::set_visible_range`] frames a range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleRangeOptions {
    /// Keep the configured right margin (whitespace past `range.to`) inside the frame.
    #[serde(default)]
    pub right_margin: bool,
}

/// How [`ChartRenderer::set_bars`] treats the viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SetBarsOptions {
    /// Keep the current viewport: used when extending the same series in place
    /// (swapping a quick preview for the full history) so candles do not jump.
    /// The default re-frames the view, as for a fresh series.
    pub preserve_view: bool,
}

/// A direction to move a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneMoveDir {
    Up,
    Down,
}

/// A user-initiated pane action reported by the renderer (hover buttons, double-clicks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    Move {
        pane_id: String,
        dir: PaneMoveDir,
    },
    Remove {
        pane_id: String,
    },
    Collapse {
        pane_id: String,
        collapsed: bool,
    },
    Maximize {
        pane_id: String,
        maximized: bool,
    },
    /// The user finished resizing panes (a separator drag or reset); read the new
    /// weights back through [`ChartRenderer::pane_height_weight`].
    Resize {
        pane_ids: Vec<String>,
    },
}

impl PaneAction {
    /// The original's `type` discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Move { .. } => "move",
            Self::Remove { .. } => "remove",
            Self::Collapse { .. } => "collapse",
            Self::Maximize { .. } => "maximize",
            Self::Resize { .. } => "resize",
        }
    }
}

/// Where a new pane goes, relative to an existing one. Both `None`: a new pane
/// at the bottom.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPanePlacement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Where to move an indicator (`handle.moveTo(...)`).
///
/// JSON: `"price"`, `{"pane": id}`, `{"newPane": {before?, after?}}` or
/// `{"newPane": true}` (the default placement, which serializes as `true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "MoveTargetWire", into = "MoveTargetWire")]
pub enum MoveTarget {
    /// Merge into the main price pane (on its own scale unless it is a
    /// price-unit overlay).
    Price,
    /// Merge into an existing pane (`Pane::id`).
    Pane(String),
    /// Create a fresh pane, optionally placed relative to an existing one.
    NewPane(NewPanePlacement),
}

#[derive(Serialize, Deserialize)]
enum PriceTag {
    #[serde(rename = "price")]
    Price,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum NewPaneWire {
    Default(bool),
    Placement(NewPanePlacement),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum MoveTargetWire {
    Price(PriceTag),
    Pane {
        pane: String,
    },
    NewPane {
        #[serde(rename = "newPane")]
        new_pane: NewPaneWire,
    },
}

impl From<MoveTargetWire> for MoveTarget {
    fn from(wire: MoveTargetWire) -> Self {
        match wire {
            MoveTargetWire::Price(_) => Self::Price,
            MoveTargetWire::Pane { pane } => Self::Pane(pane),
            MoveTargetWire::NewPane {
                new_pane: NewPaneWire::Placement(placement),
            } => Self::NewPane(placement),
            MoveTargetWire::NewPane {
                new_pane: NewPaneWire::Default(_),
            } => Self::NewPane(NewPanePlacement::default()),
        }
    }
}

impl From<MoveTarget> for MoveTargetWire {
    fn from(target: MoveTarget) -> Self {
        match target {
            MoveTarget::Price => Self::Price(PriceTag::Price),
            MoveTarget::Pane(pane) => Self::Pane { pane },
            MoveTarget::NewPane(placement) => Self::NewPane {
                new_pane: if placement == NewPanePlacement::default() {
                    NewPaneWire::Default(true)
                } else {
                    NewPaneWire::Placement(placement)
                },
            },
        }
    }
}

/// The host shell's chrome size class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    /// Touch-first presentation: fullscreen dialogs, no hover-gated affordances.
    Mobile,
    /// The pointer-first presentation (the default).
    #[default]
    Desktop,
}

/// One host-app settings tab of the renderer's settings dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSection {
    pub title: MessageLike,
    /// Callback rows: opaque, forwarded as the host built them.
    pub rows: Vec<Value>,
    /// Names the section for the visibility policy (defaults to the title's slug).
    pub id: Option<String>,
}

/// Settings-dialog visibility policy: `hidden` lists setting ids to hide (a tab,
/// a group or a row, with subtree semantics). Presentation only: hidden values
/// keep being stored and applied.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsVisibility {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Vec<String>>,
}

// ── the user-drawings port (drawings/port.ts) ──

/// The renderer-local drawing MODES beyond an armed tool: the transient measure
/// ruler or the eraser. "No mode" is `None` where the original has `null`
/// (`Option<DrawingMode>`). Mutually exclusive with each other and with any
/// armed tool: the renderer owns that exclusion, the core mirrors the outcome
/// (the `mode` intent) so external UIs can reflect and drive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrawingMode {
    Measure,
    Eraser,
}

/// Where a `reorder` intent sends a drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReorderTo {
    Front,
    Back,
}

/// Renderer to core INTENT. The renderer proposes a change from a user gesture;
/// the core `DrawingController` decides, mutates the store (the source of
/// truth), re-syncs, and emits a `drawing:*` event. One enum (not one callback
/// per kind) because every arm routes to the same destination.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawingIntent {
    /// Arm a tool (`None` = back to the pointer).
    Arm {
        type_key: Option<DrawingTypeKey>,
    },
    /// Placement in progress: the ghost's current shape after every anchor click
    /// and cursor move; `None` when placement ends (finalized OR cancelled). No
    /// store mutation: the core only re-emits it (`drawing:draft`) so a
    /// multi-chart host can mirror the ghost live. A renderer that never emits
    /// it simply syncs at completion.
    Draft {
        doc: Option<SerializedDrawing>,
    },
    Create {
        doc: SerializedDrawing,
    },
    Edit {
        doc: SerializedDrawing,
    },
    /// Atomic multi-drag / multi-nudge (one undo entry).
    EditMany {
        docs: Vec<SerializedDrawing>,
    },
    /// `additive`: shift-toggle instead of replace.
    Select {
        ids: Vec<String>,
        additive: bool,
    },
    Delete {
        ids: Vec<String>,
    },
    Reorder {
        id: String,
        to: ReorderTo,
    },
    Settings {
        id: String,
    },
    ToolFinished {
        type_key: DrawingTypeKey,
    },
    /// A flyout star was toggled.
    Favorite {
        type_key: DrawingTypeKey,
        on: bool,
    },
    /// The settings panel saved the drawing's look as a named style for its tool.
    TemplateSave {
        type_key: DrawingTypeKey,
        name: String,
        settings: DrawingToolDefaults,
    },
    /// The settings panel deleted one of a tool's saved styles.
    TemplateRemove {
        type_key: DrawingTypeKey,
        name: String,
    },
    /// The renderer-local magnet state changed (in-chart toolbar click, or a
    /// mutual-exclusion side effect). The core mirrors the value and re-emits it
    /// as a chart event; an equal value is a no-op, which keeps the
    /// command/intent loop convergent.
    SnapMode {
        mode: SnapMode,
    },
    /// Stay-in-drawing-mode toggled in-chart: when on, finishing a drawing
    /// leaves the tool armed instead of reverting to the pointer.
    StayMode {
        on: bool,
    },
    /// The renderer-local mode changed (arming a tool exits measure/eraser).
    Mode {
        mode: Option<DrawingMode>,
    },
    Undo,
    Redo,
    /// Clone in place and select the clones.
    Duplicate {
        ids: Vec<String>,
    },
    /// Commit COPIES carrying the geometry given (the end of a drag-to-duplicate:
    /// the sources never moved, the docs are their serialized twins already
    /// translated). Ids are reassigned; the copies keep their sources' depth and
    /// become the selection. One undo step.
    Clone {
        docs: Vec<SerializedDrawing>,
    },
    Copy {
        ids: Vec<String>,
    },
    Paste,
}

impl DrawingIntent {
    /// The original's `kind` discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Arm { .. } => "arm",
            Self::Draft { .. } => "draft",
            Self::Create { .. } => "create",
            Self::Edit { .. } => "edit",
            Self::EditMany { .. } => "edit-many",
            Self::Select { .. } => "select",
            Self::Delete { .. } => "delete",
            Self::Reorder { .. } => "reorder",
            Self::Settings { .. } => "settings",
            Self::ToolFinished { .. } => "tool-finished",
            Self::Favorite { .. } => "favorite",
            Self::TemplateSave { .. } => "template-save",
            Self::TemplateRemove { .. } => "template-remove",
            Self::SnapMode { .. } => "snap-mode",
            Self::StayMode { .. } => "stay-mode",
            Self::Mode { .. } => "mode",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Duplicate { .. } => "duplicate",
            Self::Clone { .. } => "clone",
            Self::Copy { .. } => "copy",
            Self::Paste => "paste",
        }
    }
}

/// The pane's SERIES stack in z terms, for renderers whose drawings share one
/// draw-order space with the series (`drawing_depth`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StackRange {
    /// "Bring to front" beats `front`.
    pub front: f64,
    /// "Send to back" undercuts `back`.
    pub back: f64,
    /// The candles' own key (absent on a study pane): a new drawing starts just under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
}

/// The interactive user-drawings surface a renderer optionally implements.
/// Present iff `capabilities().user_drawings`. Commands flow down; one intent
/// channel flows up. Only plain [`SerializedDrawing`] / toolbar data crosses: no
/// backend types.
pub trait DrawingsRendererPort {
    /// Hand the renderer the inert toolbar definition to RENDER (groups/tools/icons).
    fn set_toolbar(&self, def: &ToolbarDefinition);
    /// Show or hide the on-chart drawing toolbar.
    fn show_toolbar(&self, visible: bool);
    /// Push the authoritative snapshot down; the renderer re-projects and repaints.
    fn sync_drawings(&self, docs: &[SerializedDrawing]);
    /// Arm or disarm a tool (`None` = selection/idle, pan resumes). `last_style`
    /// is the tool's last-used style (if any) so the placement preview matches
    /// what will be committed, rather than falling back to the type default;
    /// `defaults` carries the tool's whole remembered settings (style, text
    /// styling, per-type settings) for a renderer that previews more than the
    /// style.
    fn set_active_tool(
        &self,
        tool: Option<&str>,
        last_style: Option<&DrawingStyle>,
        defaults: Option<&DrawingToolDefaults>,
    );
    /// Reflect which drawings are selected (drives handle painting); empty = none.
    fn set_selection(&self, ids: &[String]);
    /// Push the FAVORITE tool set (flyout stars and any favorites-driven UI).
    /// Optional: favorites still work headless without a renderer reflection.
    fn set_favorites(&self, _tools: &[DrawingTypeKey]) {}
    /// Push every tool's saved looks (the settings panel's style menu). Optional:
    /// the styles still round-trip through the core without a renderer reflection.
    fn set_tool_templates(&self, _map: &BTreeMap<DrawingTypeKey, Vec<DrawingToolTemplate>>) {}
    /// Push per-tool shortcut hints: PRE-FORMATTED display strings (`Alt+T`)
    /// shown beside the tools in the toolbar flyouts. The host owns the keymap
    /// and the platform formatting; the renderer only displays. Optional.
    fn set_tool_shortcuts(&self, _map: &BTreeMap<DrawingTypeKey, String>) {}
    /// Set the sticky magnet snap mode. Optional: a renderer without a magnet
    /// omits it; the in-chart toolbar reflects the pushed value.
    fn set_snap_mode(&self, _mode: SnapMode) {}
    /// Set stay-in-drawing-mode (tools remain armed after each placement). Optional.
    fn set_stay_mode(&self, _on: bool) {}
    /// Enter or exit a renderer-local mode (measure ruler / eraser; `None`
    /// exits). The renderer keeps owning the mutual exclusion (with armed tools
    /// too) and reports every actual change back through the `Mode` intent. Optional.
    fn set_mode(&self, _mode: Option<DrawingMode>) {}
    /// Hand the renderer the core's series gateway so data-driven drawings can
    /// read bars of a finer timeframe (exposed to them as
    /// `Projector::series_in_range`). Optional: a renderer without it simply
    /// never resolves lower-timeframe series.
    fn set_series_gateway(&self, _gateway: Rc<dyn DrawingSeriesGateway>) {}
    /// Open a drawing's settings popup (selecting it too): the programmatic twin
    /// of a click on it.
    fn open_settings(&self, id: &str);
    /// Display another chart's in-progress placement as a GHOST at reduced
    /// opacity (`None` clears it): the drawings-sync twin of
    /// [`ChartRenderer::set_external_crosshair`]. Never a store drawing: no
    /// selection, no hit-testing, no persistence. Optional.
    fn set_external_ghost(&self, _doc: Option<&SerializedDrawing>) {}
    /// The pane's SERIES stack in z terms (see [`StackRange`]). Optional: without
    /// it drawings order only among themselves, on a layer of their own.
    fn stack_range(&self, _pane_id: &str) -> Option<StackRange> {
        None
    }
    /// The one channel up: create/edit/select/delete/settings/tool-finished.
    fn on_drawing_intent(&self, cb: Listener<DrawingIntent>) -> Unsubscribe;
}

// ── the renderer ──

/// The rendering backend abstraction. No backend types cross this boundary:
/// that is what makes the renderer swappable. Shared as `Rc<dyn ChartRenderer>`.
///
/// Required methods are the ones the original declares without `?`; every other
/// method has a "not supported" default (see the module doc).
pub trait ChartRenderer {
    fn capabilities(&self) -> RendererCapabilities;

    /// Stable identity of this renderer (`native`, `lwc`): the warn label and
    /// `chart.renderer.name`.
    fn name(&self) -> &str;
    /// Feature keys this renderer can get/set at runtime via
    /// `chart.renderer.set` / `.get`.
    fn features(&self) -> Vec<String>;
    /// Apply a supported feature live (the caller has already checked
    /// [`features`](Self::features)); repaints as needed.
    fn apply_feature(&self, key: &str, value: &Value);
    /// Read a feature's current value (`None` = undefined: unsupported).
    fn read_feature(&self, key: &str) -> Option<Value>;

    /// Attach to the host surface and paint with `theme`. The original also
    /// takes the container element; core has no DOM, so a DOM-owning renderer is
    /// given its container at construction (see the module doc).
    fn mount(&self, theme: &VelaTheme);
    fn set_theme(&self, theme: &VelaTheme);
    /// Adopt the chart's localization: render text through `i18n` and numbers
    /// and dates through `i18n.format()`, then repaint. Called once after
    /// `mount` and again after every `chart.setLocale`. `root` carries what the
    /// renderer's root element should hold (`None` = leave the attribute alone).
    /// Optional: a renderer without it keeps English text and formatting.
    fn set_localization(&self, _i18n: Rc<dyn Translator>, _root: RendererRootAttributes) {}
    fn resize(&self);
    fn destroy(&self);

    /// Replace all price bars. By default re-frames the view (a fresh series);
    /// with [`SetBarsOptions::preserve_view`] it keeps the current viewport, used
    /// when extending the same series in place so candles do not jump.
    fn set_bars(&self, bars: &[Ohlcv], opts: SetBarsOptions);
    /// Append a new bar or replace the forming (last) bar, decided by time.
    fn update_bar(&self, bar: &Ohlcv);

    /// Set the active symbol's tick size (`0.01`) so the price axis renders the
    /// instrument's true precision. Passed as soon as symbol metadata resolves;
    /// `None` makes the renderer fall back to its zoom-derived decimals.
    /// Optional; callers test [`supports_set_price_precision`](Self::supports_set_price_precision).
    fn set_price_precision(&self, _mintick: Option<f64>) {}
    fn supports_set_price_precision(&self) -> bool {
        false
    }

    /// Set the facts about the active symbol that decide which price styles can
    /// draw it (`SymbolFacts`, derived from `SymbolInfo`): passed when they land
    /// and again on every symbol change, `None` while unknown. A renderer draws a
    /// style the facts rule out (candles for `ohlc: false`) with a close-based
    /// fallback instead, keeping the chosen style as its `priceStyle`, and greys
    /// such styles in its own pickers. Optional: a renderer without it draws
    /// whatever style is set.
    fn set_symbol_facts(&self, _facts: Option<SymbolFacts>) {}

    /// Push a bespoke NATIVE-LAYER render payload, keyed by layer `layer_type`.
    /// Visuals that are not ordinary series/fills draw through a dedicated
    /// renderer layer instead of the model; this is the channel. Producers are
    /// native indicators (`volume`, `vpvr`: layer config) and core data engines
    /// behind a price style. Optional; callers test
    /// [`supports_set_native_data`](Self::supports_set_native_data).
    fn set_native_data(&self, _layer_type: &str, _data: &Value) {}
    fn supports_set_native_data(&self) -> bool {
        false
    }

    /// Replace the timeline marks and their group definitions (the `chart.marks`
    /// model). The renderer snaps each mark onto its bar, folds same-bar /
    /// same-group marks into clusters, and paints the lane above the time axis;
    /// group visibility is its own display state (the `marks` feature). Present
    /// iff `capabilities().timeline_marks`; callers test
    /// [`supports_set_timeline_marks`](Self::supports_set_timeline_marks).
    fn set_timeline_marks(&self, _marks: &[TimelineMark], _groups: &[MarkGroup]) {}
    fn supports_set_timeline_marks(&self) -> bool {
        false
    }
    /// A timeline-mark glyph was clicked; every mark of its cluster is listed.
    /// Present iff `capabilities().timeline_marks`. `None` = no such event.
    fn on_mark_click(&self, _cb: Listener<MarkClickEvent>) -> Option<Unsubscribe> {
        None
    }

    /// Reflect an indicator's live status in its legend row: `Loading` (a fetch
    /// is in flight: spinner), `Live` (live-updating: a distinct pulse) or `Idle`.
    /// Optional.
    fn set_indicator_status(&self, _handle: &IndicatorRenderHandle, _status: IndicatorStatus) {}

    /// A market load is in flight with NO bars painted yet: the first load, or a
    /// symbol/timeframe switch (the host clears the old series first). Renderers
    /// may show a subtle loading affordance, and must hide any content that does
    /// NOT ride the bar series (corner-anchored tables); bar-mapped content
    /// vanishes with the cleared series on its own. The host turns the flag off
    /// with the first series it hands over (or when a load fails or parks); the
    /// core also mirrors this state to plugins as `load:start` / `load:end`.
    /// Optional.
    fn set_loading(&self, _loading: bool) {}

    fn ensure_pane(&self, pane: &Pane);
    fn remove_pane(&self, id: &str);

    /// Move a mounted indicator to another pane (merge/unmerge). `own_scale`
    /// gives the indicator its own scale column within the target pane, rescaled
    /// so its visible extent lines up with the pane's; false shares the pane
    /// scale. Present iff `capabilities().pane_management`; callers test
    /// [`supports_set_indicator_pane`](Self::supports_set_indicator_pane).
    fn set_indicator_pane(
        &self,
        _handle: &IndicatorRenderHandle,
        _pane_id: &str,
        _own_scale: bool,
    ) {
    }
    fn supports_set_indicator_pane(&self) -> bool {
        false
    }
    /// Set the top-to-bottom pane display order (pane ids).
    fn order_panes(&self, _ordered_ids: &[String]) {}
    /// Collapse a pane to a thin strip (legend + expand button) or restore it.
    fn set_pane_collapsed(&self, _pane_id: &str, _collapsed: bool) {}
    /// Maximize one pane to fill the plot, or restore the previous split (`None`).
    fn set_pane_maximized(&self, _pane_id: Option<&str>) {}
    /// Set a pane's height weight: panes share the plot height in proportion to
    /// their weights (collapsed panes aside). The weight must survive later
    /// `ensure_pane` calls that carry none: only an explicit `height_weight`
    /// replaces it. Callers test
    /// [`supports_set_pane_height_weight`](Self::supports_set_pane_height_weight).
    fn set_pane_height_weight(&self, _pane_id: &str, _weight: f64) {}
    fn supports_set_pane_height_weight(&self) -> bool {
        false
    }
    /// A pane's current height weight, or `None` when the renderer has no such
    /// pane (or no weights at all).
    fn pane_height_weight(&self, _pane_id: &str) -> Option<f64> {
        None
    }
    /// The renderer reports a user-initiated pane action (from a pane hover
    /// button or a double-click). The host/core reflects it: `Remove` tears down
    /// the pane's indicators; the rest are already applied by the renderer and
    /// just keep core's `chart.panes` view in sync. Present iff
    /// `capabilities().pane_management`.
    fn on_pane_action(&self, _cb: Listener<PaneAction>) -> Option<Unsubscribe> {
        None
    }
    /// The renderer reports a user request to move/merge an indicator to another
    /// pane, made from its own in-chart UI (the legend "Move to" menu or a
    /// legend-row drag). Core routes it through `moveIndicator`. The callback
    /// receives the indicator id and the target. Present iff
    /// `capabilities().pane_management`.
    fn on_move_indicator(&self, _cb: MoveIndicatorListener) -> Option<Unsubscribe> {
        None
    }

    /// Mount an indicator's series and its in-chart legend/settings UI.
    fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle;
    fn update_indicator(&self, handle: &IndicatorRenderHandle, patch: &ScenePatch);
    fn remove_indicator(&self, handle: &IndicatorRenderHandle);
    /// Reflect a programmatic input change in the renderer's settings UI. `props`
    /// (when given) refreshes the declaration-prop values the same way.
    fn set_indicator_inputs(
        &self,
        handle: &IndicatorRenderHandle,
        values: &Inputs,
        props: Option<&Inputs>,
    );

    /// Supply a symbol picker so the settings dialog's `input.symbol` control
    /// opens the host's own ticker-selection UI. Optional: without it
    /// `input.symbol` is a plain text field. `None` detaches. Callers test
    /// [`supports_set_symbol_picker`](Self::supports_set_symbol_picker).
    fn set_symbol_picker(&self, _picker: Option<SymbolPickerFn>) {}
    fn supports_set_symbol_picker(&self) -> bool {
        false
    }

    /// Supply the HOST-CONTRIBUTED actions of each legend row. The provider is
    /// called per row, lazily, so `when()` gates and late registrations resolve
    /// at render time; calling this again replaces the provider AND re-projects
    /// the rows already on screen. Optional: a renderer without it never shows
    /// contributed legend actions.
    fn set_legend_actions(&self, _provider: Option<LegendActionProvider>) {}

    /// Supply the HOST-CONTRIBUTED callout bubbles of each legend row. Same
    /// contract as [`set_legend_actions`](Self::set_legend_actions). Optional.
    fn set_legend_callouts(&self, _provider: Option<LegendCalloutProvider>) {}

    /// Replace the indicator legend's fold toggle with a HOST action (or restore
    /// it with `None`): the chip stays (a list glyph plus the indicator count)
    /// but pressing it runs the action instead of unfolding the rows, which stay
    /// hidden while the override is in force. Multi-chart shells route it to
    /// their indicator overview. Optional.
    fn set_legend_overview_action(&self, _action: Option<Rc<dyn Fn()>>) {}

    /// Open the settings dialog of one mounted indicator: the programmatic twin
    /// of the legend row's gear button. Silent no-op for an unknown id. Optional;
    /// callers test [`supports_open_indicator_settings`](Self::supports_open_indicator_settings).
    fn open_indicator_settings(&self, _indicator_id: &str) {}
    fn supports_open_indicator_settings(&self) -> bool {
        false
    }

    /// Hide (`false`) or show (`true`) a mounted indicator's visuals while
    /// keeping its legend row (marked hidden). Optional: a renderer that cannot
    /// suppress an indicator omits it (the core's hide still works for resource
    /// suspension; only the in-chart legend eye is unavailable). On show the core
    /// re-mounts the indicator, so this need only drop the visuals and flag the row.
    fn set_indicator_visible(&self, _handle: &IndicatorRenderHandle, _visible: bool) {}

    /// The renderer tells core when the base price style it DRAWS changes, from
    /// ANY source (the `priceStyle` feature, the in-chart settings dialog, an
    /// applied config template, symbol facts that rule the chosen style out).
    /// Display state stays renderer-owned; the core listens because some styles
    /// carry a DATA requirement. Optional.
    fn on_price_style_change(&self, _cb: Listener<PriceStyle>) -> Option<Unsubscribe> {
        None
    }

    /// The renderer tells core that the base price style is ABOUT to change,
    /// `from` to `to` (the callback receives `(from, to)`): the same sources as
    /// [`on_price_style_change`](Self::on_price_style_change) (one runtime write
    /// path), called synchronously before the renderer touches its scene, so the
    /// chart still paints `from` while the callback runs. Never called for the
    /// constructed style or for a write of the style already shown. Callbacks must
    /// not change the style themselves. Optional.
    fn on_price_style_will_change(&self, _cb: PriceStyleWillChangeListener) -> Option<Unsubscribe> {
        None
    }

    /// The renderer tells core when the user edits an input in-chart.
    fn on_input_change(&self, cb: Listener<InputChangeEvent>) -> Unsubscribe;
    /// The renderer tells core when the user removes an indicator in-chart (the
    /// legend close button); the callback receives the indicator id.
    fn on_remove_indicator(&self, cb: Listener<String>) -> Unsubscribe;
    /// The renderer tells core when the user toggles an indicator's visibility
    /// in-chart (the legend eye); the callback receives `(id, visible)`. Optional.
    fn on_toggle_indicator_visible(&self, _cb: ToggleVisibleListener) -> Option<Unsubscribe> {
        None
    }
    fn on_crosshair_move(&self, cb: Listener<CrosshairEvent>) -> Unsubscribe;
    fn on_click(&self, cb: Listener<ClickEvent>) -> Unsubscribe;
    /// Touch long-press on a price or time axis strip. Optional: host chrome
    /// (timezone / price-scale sheets) keys off it.
    fn on_axis_long_press(&self, _cb: Listener<AxisLongPressEvent>) -> Option<Unsubscribe> {
        None
    }

    /// Display an EXTERNAL crosshair at a data-space position: a ghost marker
    /// driven by another chart (multi-chart crosshair sync), not by this chart's
    /// own pointer. `time` is epoch-ms (`None` clears); `price` optionally adds
    /// the horizontal line when the caller knows the scales are comparable
    /// (same-symbol groups). The ghost must NEVER re-emit `on_crosshair_move`:
    /// that one-way rule is what makes the sync loop-free. Optional; detect by
    /// [`supports_set_external_crosshair`](Self::supports_set_external_crosshair).
    fn set_external_crosshair(&self, _time: Option<Millis>, _price: Option<f64>) {}
    fn supports_set_external_crosshair(&self) -> bool {
        false
    }

    /// A pre-formatted readout of the bar under the crosshair, or of the latest
    /// bar when the cursor is off the plot, so the snapshot is always useful.
    /// Pull it on crosshair movement to drive a host data-window panel. Optional:
    /// a renderer that tracks no hovered bar returns `None`.
    fn get_data_window_readout(&self) -> Option<DataWindowReadout> {
        None
    }

    fn get_visible_range(&self) -> Option<VisibleRange>;
    /// Frame `range` across the plot width. With `opts.right_margin` the renderer
    /// also leaves its configured right margin (whitespace past `range.to`, in
    /// bars) inside the frame: how a named preset frames the newest bars, so a
    /// preset (and "Reset chart view") never drops that margin. A renderer
    /// without a right margin ignores the option.
    fn set_visible_range(&self, range: VisibleRange, opts: VisibleRangeOptions);
    /// Pan by a fraction of the visible width at constant zoom (positive: toward
    /// the latest bars), behaving exactly like a pointer drag: same pan limits
    /// (including the bounded whitespace past the newest bar), eased if the
    /// renderer animates pans. Optional: without it
    /// ([`supports_pan_by`](Self::supports_pan_by) false) the core falls back to
    /// an instant `set_visible_range` shift.
    fn pan_by(&self, _fraction: f64) {}
    fn supports_pan_by(&self) -> bool {
        false
    }
    fn on_viewport_change(&self, cb: Listener<VisibleRange>) -> Unsubscribe;

    /// Export the current chart as a PNG data URL. Optional: a renderer that
    /// cannot rasterize its surface returns `None`. DOM overlays may not be
    /// included. See also [`supports_screenshot`](Self::supports_screenshot).
    fn screenshot(&self) -> Option<String> {
        None
    }
    fn supports_screenshot(&self) -> bool {
        false
    }

    /// Snapshot the renderer's full cosmetic configuration as a serializable,
    /// versioned document, for persistence (templates, saved user settings).
    /// Optional: a renderer without a rich config returns `None`. The value is
    /// plain JSON; its concrete shape is renderer-defined.
    fn get_config(&self) -> Option<Value> {
        None
    }
    /// Apply a (possibly partial) config document produced by `get_config`.
    /// Implementations validate untrusted input and ignore unknown/malformed
    /// fields, repainting with no indicator re-run. Callers test
    /// [`supports_apply_config`](Self::supports_apply_config).
    fn apply_config(&self, _config: &Value) {}
    fn supports_apply_config(&self) -> bool {
        false
    }
    /// The renderer's cosmetic config changed via `apply_config` (the in-chart
    /// settings dialog commits through it), so this is how host chrome mirroring
    /// a config value learns about in-chart edits. Re-pull `get_config` /
    /// `read_feature` for the new values. Optional, paired with `apply_config`.
    fn on_config_changed(&self, _cb: Box<dyn FnMut()>) -> Option<Unsubscribe> {
        None
    }

    /// The renderer reports a user request to switch the APP THEME, made from its
    /// own in-chart UI (the settings dialog's Canvas, Theme row): a built-in
    /// theme's name or a registered theme's id. The core owns the canonical
    /// theme: it resolves the id, calls [`set_theme`](Self::set_theme), and emits
    /// `theme:changed` so host chrome follows. Optional.
    fn on_theme_select(&self, _cb: Listener<ThemeId>) -> Option<Unsubscribe> {
        None
    }

    /// Move keyboard focus onto the chart's interactive surface. Optional: a host
    /// UI calls it after its own controls steal focus so chart/drawing keys keep
    /// working.
    fn focus(&self) {}

    /// Close any in-chart dialogs the renderer owns (the indicator settings
    /// dialog, the chart-settings gear dialog). Optional: used by a host to keep
    /// its own dialogs mutually exclusive with the renderer's. A no-op when
    /// nothing is open.
    fn close_dialogs(&self) {}
    /// Open (or toggle) the renderer's own settings dialog, when it has one.
    /// `section` names the tab to land on (matched against the dialog's section
    /// titles, unknown ones ignored); with a section an already-open dialog
    /// switches tab rather than closing. Optional.
    fn open_settings_dialog(&self, _section: Option<&str>) {}
    /// A chart type's SDK settings changed (dialog edit / `apply_config`); the
    /// core forwards them to the type's data engine. The callback receives
    /// `(type_id, values)`. Optional.
    fn on_chart_type_settings_change(&self, _cb: ChartTypeSettingsListener) -> Option<Unsubscribe> {
        None
    }
    /// Host-app settings tabs (callback rows) shown by the renderer's settings
    /// dialog. Optional.
    fn set_settings_sections(&self, _sections: &[SettingsSection]) {}
    /// Settings-dialog visibility policy (see [`SettingsVisibility`]). Optional:
    /// a renderer without a settings dialog omits it.
    fn set_settings_visibility(&self, _policy: &SettingsVisibility) {}
    /// Every addressable setting id of this chart (built-ins, chart-type
    /// sections, host sections): the discovery surface for the visibility policy.
    /// Empty when unsupported.
    fn list_settings_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// The host shell's chrome size class. `Mobile` asks the renderer's own
    /// chrome (dialogs, toolbars, buttons) for its touch-first presentation;
    /// `Desktop` (the default) keeps the pointer-first one. Optional.
    fn set_layout_mode(&self, _mode: LayoutMode) {}

    /// Drive the renderer's time-of-day displays (the countdown-to-bar-close
    /// chip) from the HOST's second pulse instead of the renderer's own, so they
    /// read the same second as the host's clock chrome. `None` restores the
    /// renderer's own pulse. Optional.
    fn set_wall_clock(&self, _clock: Option<Rc<dyn WallClock>>) {}

    /// Interactive user-drawings surface. `Some` iff `capabilities().user_drawings`.
    /// The core `DrawingController` drives it (commands down) and listens for
    /// intents (up); a renderer without drawing tools returns `None`.
    fn user_drawings_port(&self) -> Option<Rc<dyn DrawingsRendererPort>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::runtime::noop_unsubscribe;
    use serde_json::json;

    /// Implements only the required methods.
    struct Bare;

    impl ChartRenderer for Bare {
        fn capabilities(&self) -> RendererCapabilities {
            RendererCapabilities {
                panes: true,
                pane_management: false,
                fills: Support::Unsupported,
                bgcolor: Support::Native,
                hline: Support::Primitive,
                markers: false,
                barcolor: BarColorSupport::Approximated,
                per_point_color: false,
                drawings: false,
                user_drawings: false,
                drawing_depth: false,
                tables: false,
                trades: false,
                timeline_marks: false,
                inputs_ui: false,
            }
        }
        fn name(&self) -> &str {
            "bare"
        }
        fn features(&self) -> Vec<String> {
            Vec::new()
        }
        fn apply_feature(&self, _key: &str, _value: &Value) {}
        fn read_feature(&self, _key: &str) -> Option<Value> {
            None
        }
        fn mount(&self, _theme: &VelaTheme) {}
        fn set_theme(&self, _theme: &VelaTheme) {}
        fn resize(&self) {}
        fn destroy(&self) {}
        fn set_bars(&self, _bars: &[Ohlcv], _opts: SetBarsOptions) {}
        fn update_bar(&self, _bar: &Ohlcv) {}
        fn ensure_pane(&self, _pane: &Pane) {}
        fn remove_pane(&self, _id: &str) {}
        fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
            IndicatorRenderHandle {
                id: model.id.clone(),
            }
        }
        fn update_indicator(&self, _handle: &IndicatorRenderHandle, _patch: &ScenePatch) {}
        fn remove_indicator(&self, _handle: &IndicatorRenderHandle) {}
        fn set_indicator_inputs(
            &self,
            _handle: &IndicatorRenderHandle,
            _values: &Inputs,
            _props: Option<&Inputs>,
        ) {
        }
        fn on_input_change(&self, _cb: Listener<InputChangeEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_remove_indicator(&self, _cb: Listener<String>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_crosshair_move(&self, _cb: Listener<CrosshairEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_click(&self, _cb: Listener<ClickEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn get_visible_range(&self) -> Option<VisibleRange> {
            None
        }
        fn set_visible_range(&self, _range: VisibleRange, _opts: VisibleRangeOptions) {}
        fn on_viewport_change(&self, _cb: Listener<VisibleRange>) -> Unsubscribe {
            noop_unsubscribe()
        }
    }

    #[test]
    fn a_renderer_with_only_the_required_methods_is_object_safe() {
        let renderer: Rc<dyn ChartRenderer> = Rc::new(Bare);
        assert_eq!(renderer.name(), "bare");
        assert!(!renderer.supports_pan_by());
        assert!(!renderer.supports_set_native_data());
        assert!(!renderer.supports_screenshot());
        assert!(renderer.screenshot().is_none());
        assert!(renderer.get_config().is_none());
        assert!(renderer.get_data_window_readout().is_none());
        assert!(renderer.pane_height_weight("p").is_none());
        assert!(renderer.list_settings_ids().is_empty());
        assert!(renderer.user_drawings_port().is_none());
        assert!(renderer.on_pane_action(Box::new(|_| {})).is_none());
        assert!(renderer.on_price_style_change(Box::new(|_| {})).is_none());
        assert!(renderer.on_mark_click(Box::new(|_| {})).is_none());
        renderer.set_pane_maximized(None);
        renderer.set_loading(true);
        let by_ref: &dyn ChartRenderer = &Bare;
        assert!(by_ref.capabilities().panes);
    }

    struct BareDrawings;

    impl DrawingsRendererPort for BareDrawings {
        fn set_toolbar(&self, _def: &ToolbarDefinition) {}
        fn show_toolbar(&self, _visible: bool) {}
        fn sync_drawings(&self, _docs: &[SerializedDrawing]) {}
        fn set_active_tool(
            &self,
            _tool: Option<&str>,
            _last_style: Option<&DrawingStyle>,
            _defaults: Option<&DrawingToolDefaults>,
        ) {
        }
        fn set_selection(&self, _ids: &[String]) {}
        fn open_settings(&self, _id: &str) {}
        fn on_drawing_intent(&self, _cb: Listener<DrawingIntent>) -> Unsubscribe {
            noop_unsubscribe()
        }
    }

    #[test]
    fn a_drawings_port_with_only_the_required_methods_is_object_safe() {
        let port: Rc<dyn DrawingsRendererPort> = Rc::new(BareDrawings);
        assert!(port.stack_range("price").is_none());
        port.set_mode(Some(DrawingMode::Eraser));
        port.set_external_ghost(None);
        port.on_drawing_intent(Box::new(|_| {}))();
    }

    #[test]
    fn a_renderer_can_expose_its_drawings_port() {
        struct WithDrawings(Rc<BareDrawings>);
        impl ChartRenderer for WithDrawings {
            fn capabilities(&self) -> RendererCapabilities {
                Bare.capabilities()
            }
            fn name(&self) -> &str {
                "with-drawings"
            }
            fn features(&self) -> Vec<String> {
                Vec::new()
            }
            fn apply_feature(&self, _: &str, _: &Value) {}
            fn read_feature(&self, _: &str) -> Option<Value> {
                None
            }
            fn mount(&self, _: &VelaTheme) {}
            fn set_theme(&self, _: &VelaTheme) {}
            fn resize(&self) {}
            fn destroy(&self) {}
            fn set_bars(&self, _: &[Ohlcv], _: SetBarsOptions) {}
            fn update_bar(&self, _: &Ohlcv) {}
            fn ensure_pane(&self, _: &Pane) {}
            fn remove_pane(&self, _: &str) {}
            fn mount_indicator(&self, m: &IndicatorModel) -> IndicatorRenderHandle {
                IndicatorRenderHandle { id: m.id.clone() }
            }
            fn update_indicator(&self, _: &IndicatorRenderHandle, _: &ScenePatch) {}
            fn remove_indicator(&self, _: &IndicatorRenderHandle) {}
            fn set_indicator_inputs(
                &self,
                _: &IndicatorRenderHandle,
                _: &Inputs,
                _: Option<&Inputs>,
            ) {
            }
            fn on_input_change(&self, _: Listener<InputChangeEvent>) -> Unsubscribe {
                noop_unsubscribe()
            }
            fn on_remove_indicator(&self, _: Listener<String>) -> Unsubscribe {
                noop_unsubscribe()
            }
            fn on_crosshair_move(&self, _: Listener<CrosshairEvent>) -> Unsubscribe {
                noop_unsubscribe()
            }
            fn on_click(&self, _: Listener<ClickEvent>) -> Unsubscribe {
                noop_unsubscribe()
            }
            fn get_visible_range(&self) -> Option<VisibleRange> {
                None
            }
            fn set_visible_range(&self, _: VisibleRange, _: VisibleRangeOptions) {}
            fn on_viewport_change(&self, _: Listener<VisibleRange>) -> Unsubscribe {
                noop_unsubscribe()
            }
            fn user_drawings_port(&self) -> Option<Rc<dyn DrawingsRendererPort>> {
                Some(self.0.clone())
            }
        }
        let renderer = WithDrawings(Rc::new(BareDrawings));
        assert!(renderer.user_drawings_port().is_some());
    }

    #[test]
    fn listeners_are_stored_and_invoked_by_the_renderer() {
        // A renderer keeps the callback and calls it later; Unsubscribe detaches it.
        struct Hub(RefCell<Vec<Listener<ClickEvent>>>);
        let hub = Hub(RefCell::new(Vec::new()));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        hub.0
            .borrow_mut()
            .push(Box::new(move |e| sink.borrow_mut().push(e.price)));
        let click = ClickEvent {
            price: Some(1.5),
            ..ClickEvent::default()
        };
        for cb in hub.0.borrow_mut().iter_mut() {
            cb(&click);
        }
        assert_eq!(*seen.borrow(), [Some(1.5)]);
    }

    #[test]
    fn capabilities_round_trip_and_default_the_optional_flags() {
        let caps = Bare.capabilities();
        let json = serde_json::to_value(caps).unwrap();
        assert_eq!(json["paneManagement"], json!(false));
        assert_eq!(json["fills"], json!("unsupported"));
        assert_eq!(json["barcolor"], json!("approximated"));
        assert_eq!(
            serde_json::from_value::<RendererCapabilities>(json).unwrap(),
            caps
        );

        let minimal: RendererCapabilities = serde_json::from_value(json!({
            "panes": true, "paneManagement": true, "fills": "native", "bgcolor": "native",
            "hline": "native", "markers": true, "barcolor": "native", "perPointColor": true,
            "drawings": true, "userDrawings": true, "tables": true, "inputsUI": true
        }))
        .unwrap();
        assert!(!minimal.drawing_depth && !minimal.trades && !minimal.timeline_marks);
    }

    #[test]
    fn move_target_json_matches_the_original_shapes() {
        for (target, wire) in [
            (MoveTarget::Price, json!("price")),
            (MoveTarget::Pane("p1".into()), json!({"pane": "p1"})),
            (
                MoveTarget::NewPane(NewPanePlacement::default()),
                json!({"newPane": true}),
            ),
            (
                MoveTarget::NewPane(NewPanePlacement {
                    before: Some("p2".into()),
                    after: None,
                }),
                json!({"newPane": {"before": "p2"}}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&target).unwrap(), wire);
            assert_eq!(serde_json::from_value::<MoveTarget>(wire).unwrap(), target);
        }
        assert!(serde_json::from_value::<MoveTarget>(json!("nope")).is_err());
    }

    #[test]
    fn payload_structs_round_trip() {
        let ohlc = CrosshairOhlc {
            time: 1_700_000_000_000,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: None,
        };
        let json = serde_json::to_value(ohlc).unwrap();
        assert!(json.get("volume").is_none());
        assert_eq!(serde_json::from_value::<CrosshairOhlc>(json).unwrap(), ohlc);

        let readout = DataWindowReadout {
            date: "2026-07-03".into(),
            time: "10:00".into(),
            ohlc: Some(DataWindowOhlc {
                o: "1".into(),
                h: "2".into(),
                l: "0".into(),
                c: "1".into(),
                vol: Some("10".into()),
                up: true,
            }),
            groups: vec![DataWindowGroup {
                name: "SMA".into(),
                rows: vec![DataWindowRow {
                    label: "SMA".into(),
                    value: "1.5".into(),
                    color: "#fff".into(),
                }],
            }],
        };
        let back: DataWindowReadout =
            serde_json::from_value(serde_json::to_value(&readout).unwrap()).unwrap();
        assert_eq!(back, readout);

        let long_press = AxisLongPressEvent {
            axis: Axis::Time,
            x: 1.0,
            y: 2.0,
        };
        assert_eq!(
            serde_json::to_value(long_press).unwrap(),
            json!({"axis": "time", "x": 1.0, "y": 2.0})
        );
        assert_eq!(
            serde_json::to_value(VisibleRangeOptions { right_margin: true }).unwrap(),
            json!({"rightMargin": true})
        );
        assert_eq!(
            serde_json::to_value(DrawingMode::Measure).unwrap(),
            json!("measure")
        );
        assert_eq!(
            serde_json::from_value::<Option<DrawingMode>>(json!(null)).unwrap(),
            None
        );
        let range = StackRange {
            front: 9.0,
            back: -1.0,
            price: None,
        };
        assert_eq!(
            serde_json::from_value::<StackRange>(serde_json::to_value(range).unwrap()).unwrap(),
            range
        );
    }

    #[test]
    fn intent_and_pane_action_discriminants_match_the_original() {
        assert_eq!(DrawingIntent::Undo.kind(), "undo");
        assert_eq!(DrawingIntent::EditMany { docs: vec![] }.kind(), "edit-many");
        assert_eq!(
            DrawingIntent::ToolFinished {
                type_key: "box".into()
            }
            .kind(),
            "tool-finished"
        );
        assert_eq!(
            DrawingIntent::SnapMode {
                mode: SnapMode::Weak
            }
            .kind(),
            "snap-mode"
        );
        assert_eq!(DrawingIntent::Clone { docs: vec![] }.kind(), "clone");
        assert_eq!(PaneAction::Resize { pane_ids: vec![] }.kind(), "resize");
        assert_eq!(
            PaneAction::Move {
                pane_id: "p".into(),
                dir: PaneMoveDir::Up
            }
            .kind(),
            "move"
        );
    }

    #[test]
    fn defaults_of_small_helpers() {
        let button = LegendCalloutPanelItem::Button {
            label: "Go".into(),
            primary: false,
            close: None,
            run: Rc::new(|| {}),
        };
        assert!(button.closes_panel());
        assert!(!LegendCalloutPanelItem::Text { text: "t".into() }.closes_panel());
        let event = InputChangeEvent {
            indicator_id: "a".into(),
            key: "len".into(),
            value: InputValue::Number(14.0),
            kind: None,
        };
        assert_eq!(event.kind(), InputChangeKind::Input);
        assert_eq!(LayoutMode::default(), LayoutMode::Desktop);
        let crosshair = CrosshairEvent {
            time: None,
            price: None,
            pane_kind: None,
            pane_id: None,
            values: vec![("a".into(), 2.0)],
            ohlc: None,
        };
        assert_eq!(crosshair.value("a"), Some(2.0));
        assert_eq!(crosshair.value("b"), None);
    }
}
