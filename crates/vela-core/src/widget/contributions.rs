//! Widget CONTRIBUTIONS: the descriptor-based customization seam. Plugins (and host apps)
//! contribute ACTIONS as data (never DOM): the widget projects them into its own chrome
//! (topbar buttons, context-menu items) so any future view layer can project the same
//! descriptors.
//!
//! # Differences from the original
//!
//! - **The registries are an explicit value.** The original keeps every registry in a
//!   module global; here a [`Contributions`] holds them and the shells carry it (clones
//!   share it, as charts on one page share the globals). Register before shells are built;
//!   a shell's `refresh_actions` picks up later registrations.
//! - **The runtime surface is a trait.** [`WidgetContext`] is what the original's interface
//!   describes; the shell implements it. Descriptor callbacks take `&dyn WidgetContext`.
//! - **No DOM.** The original hands `HTMLElement`s to `dockStrip`, `dockEdge`, `host` and a
//!   panel's `mount`. Here those are opaque [`ElementHandle`]s the browser crate downcasts.
//! - **Warnings** (a refused registration) go through the [`Platform`].
//!
//! Everything inside a descriptor's `run` / `when` comes from the context argument, never
//! from an outer widget or chart reference: the context is rebuilt per invocation and is
//! what keeps descriptors working when multiple charts exist.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use serde_json::Value;

use super::shell_options::EngineFactory;
use super::topbar_composition::TOPBAR_BUILTIN_IDS;
use crate::i18n::{MessageLike, Translator, text_of};
use crate::model::inputs::Inputs;
use crate::ports::chart_renderer::{
    LegendActionProvider, LegendActionView, LegendCalloutPanel, LegendCalloutPanelItem,
    LegendCalloutProvider, LegendCalloutView,
};
use crate::ports::data_provider::SymbolDescriptor;
use crate::runtime::{LocalBoxFuture, Platform, Unsubscribe};
use crate::vela::Vela;

/// An element the browser crate owns. The core only passes it around.
pub type ElementHandle = Rc<dyn Any>;

// ── the runtime surface ────────────────────────────────────

/// What a context-menu action's `when` / `run` is told about the right-click that opened
/// the menu ([`WidgetContext::pointer`]), on the chart that received it: in a workspace,
/// the cell under the pointer. Captured once at open: moving through the menu before
/// picking a row does not change it. The axis menus get what the crosshair reports over
/// the axis: off the plot, `price` and `pane_kind` are `None`, while `time` still names the
/// bar column under the pointer (on the time axis, the bar above the click; `None` past the
/// last bar).
#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenuPointer {
    /// The value under the pointer on the pane it landed on: a price on the price pane, an
    /// indicator value on a study pane (see `pane_kind`). `None` off the plot.
    pub price: Option<f64>,
    /// Open time of the bar under the pointer; `None` off the bars.
    pub time: Option<f64>,
    /// The kind of pane the pointer is on; `None` off the plot.
    pub pane_kind: Option<PointerPaneKind>,
    /// The id of the pane the pointer is on, as `chart.panes.list()` reports it; `None` off
    /// the plot.
    pub pane_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPaneKind {
    Price,
    Study,
}

/// What [`WidgetContext::add_indicator`] takes: a named script, ready to run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExternalIndicatorEntry {
    pub name: String,
    /// The script source (the recorded undo/redo action re-adds from it).
    pub script: String,
    /// The indicator's id on the chart (see `AddIndicatorOptions::id`): what a persistence
    /// handler stores and hands back on restore so the indicator comes back under the id
    /// its document knows. Omitted: the chart mints one. Either way undo/redo re-add the
    /// indicator under the id it first ran with. An id already live on the cell's chart is
    /// rejected (a warning; nothing is added).
    pub id: Option<String>,
    /// Engine language (default: the chart's default engine).
    pub language: Option<String>,
    /// Input-value overrides applied at add time (what a persistence handler restores).
    pub inputs: Option<Inputs>,
    /// Declaration-prop overrides applied at add time.
    pub props: Option<Inputs>,
    /// Restore hidden: the indicator is suspended right after the add (legend row stays,
    /// marked hidden): a persistence handler carrying a hidden flag applies it here so the
    /// engine never spends anything on a tucked-away indicator.
    pub hidden: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Error,
}

/// Where [`WidgetContext::dock_strip`] places a strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DockPosition {
    /// Between the charts and the bottom bar.
    #[default]
    AboveBottombar,
    /// Under the bottom bar, at the foot of the chart column.
    BelowBottombar,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DockStripOptions {
    pub position: DockPosition,
}

/// The edge [`WidgetContext::dock_edge`] docks to (only the right edge exists).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockEdge {
    Right,
}

/// A docked strip: call [`undock`](Self::undock) to remove it. `cover(true)` lays the
/// strip over the whole chart column (the topbar, the charts, the strips and the bottom
/// bar) but not over a `column` panel, an edge element ([`WidgetContext::dock_edge`]) or
/// the mobile layout's bottom bar; `cover(false)` puts it back in its place in the column.
pub trait DockedStrip {
    fn undock(&self);
    fn cover(&self, on: bool);
}

/// A small count or mark on a sidebar entry.
#[derive(Clone, Debug, PartialEq)]
pub enum SidebarBadge {
    Text(String),
    Number(f64),
}

/// The shell's right-edge toolbar ([`WidgetContext::sidebar`]). Its entries are the
/// side-panel toggles and the actions registered with target `sidebar`. Every method is a
/// no-op while the shell shows no sidebar (the option is off, or the layout is mobile).
pub trait SidebarControl {
    /// Whether the shell was built with a sidebar.
    fn enabled(&self) -> bool;
    /// A small count or mark on an entry (a panel id or a sidebar action id). `None`, `0`
    /// and `""` clear it; numbers above 99 show as `99+`.
    fn set_badge(&self, id: &str, badge: Option<SidebarBadge>);
    /// Redraw the entries: re-read each sidebar action's `active` and `when`.
    fn refresh(&self);
    /// Whether an entry shows right now: placed by the host's composition, available, and
    /// not hidden at runtime.
    fn is_visible(&self, id: &str) -> bool;
    /// Hide an entry (`false`) or give it back (`true`), as the topbar's `set_visible`
    /// does. It never adds an entry the composition left out, and it is not saved with the
    /// layout: a plugin applies it again when it mounts.
    fn set_visible(&self, id: &str, visible: bool);
}

/// A built-in topbar item a plugin can hide at runtime: every built-in id but the
/// `actions` flow slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TopbarItemId {
    Symbol,
    Timeframes,
    Style,
    Layout,
    Indicators,
    UndoRedo,
    Alerts,
    Panels,
    Screenshot,
}

impl TopbarItemId {
    pub fn as_str(self) -> &'static str {
        match self {
            TopbarItemId::Symbol => "symbol",
            TopbarItemId::Timeframes => "timeframes",
            TopbarItemId::Style => "style",
            TopbarItemId::Layout => "layout",
            TopbarItemId::Indicators => "indicators",
            TopbarItemId::UndoRedo => "undo-redo",
            TopbarItemId::Alerts => "alerts",
            TopbarItemId::Panels => "panels",
            TopbarItemId::Screenshot => "screenshot",
        }
    }

    /// The item for a built-in id; `None` for `actions` and anything unknown.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "symbol" => TopbarItemId::Symbol,
            "timeframes" => TopbarItemId::Timeframes,
            "style" => TopbarItemId::Style,
            "layout" => TopbarItemId::Layout,
            "indicators" => TopbarItemId::Indicators,
            "undo-redo" => TopbarItemId::UndoRedo,
            "alerts" => TopbarItemId::Alerts,
            "panels" => TopbarItemId::Panels,
            "screenshot" => TopbarItemId::Screenshot,
            _ => return None,
        })
    }
}

/// Runtime visibility of the shell's built-in topbar items ([`WidgetContext::topbar`]). It
/// narrows the host's `topbar` composition and follows the same rule: hiding an item
/// removes its other entry points with it (its row in the mobile three-dots menu, its
/// bottom-bar stop, its keyboard chord). It never adds an item the composition left out,
/// and it is not saved with the layout: a plugin applies it again when it mounts.
pub trait TopbarItems {
    /// Whether the item shows right now: in the composition, and not hidden at runtime.
    fn is_visible(&self, id: TopbarItemId) -> bool;
    /// Hide the item (`false`) or give it back (`true`).
    fn set_visible(&self, id: TopbarItemId, visible: bool);
}

/// One panel the dock holds, as [`PanelDockView::list`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockedPanelInfo {
    pub id: String,
    /// The panel's title in the shell's language.
    pub title: String,
    pub icon: String,
    /// The shell's own panel (object tree, data window) rather than a contributed one.
    pub built_in: bool,
}

/// The side-panel dock as data ([`WidgetContext::panels`]); `toggle_panel` acts on it.
pub trait PanelDockView {
    /// Every docked panel, built-ins included, in dock order.
    fn list(&self) -> Vec<DockedPanelInfo>;
    /// The open panel's id, or `None` when the column is closed.
    fn open_id(&self) -> Option<String>;
    /// Whether the panel covers the shell's charts right now (see `maximizable`).
    fn is_maximized(&self, id: &str) -> bool;
    /// Called after a panel opens, closes, maximizes or restores, and after panels are
    /// added or removed. Returns the unsubscribe.
    fn on_did_change(&self, cb: Box<dyn Fn()>) -> Unsubscribe;
}

/// The runtime surface an action's `when`/`run` receives. The shell implements it.
pub trait WidgetContext {
    /// The CURRENT inner chart. Read it through this getter rather than capturing it: a
    /// shell may replace its chart instance, and a captured one would be destroyed.
    /// (Symbol and timeframe switches are applied IN PLACE: the instance survives them.)
    fn chart(&self) -> Vela;
    /// LIVE getters, like `chart`: they resolve the ACTIVE cell's market at every read, so
    /// an attachment that holds its mount context keeps reading the truth after a
    /// symbol/timeframe switch. Read them at the point of use; never copy them into
    /// long-lived state.
    fn symbol(&self) -> String;
    fn timeframe(&self) -> String;
    fn price_style(&self) -> String;
    fn set_symbol(&self, symbol: &str);
    fn set_timeframe(&self, tf: &str);
    fn set_price_style(&self, style: &str);
    fn open_symbol_search(&self, query: Option<&str>);
    /// Open/close a docked side panel by id (built-in or contributed): `None` flips it.
    /// The dock stays exclusive: opening one closes whichever was showing. Unknown ids are
    /// ignored. The seam a plugin uses to open ITS OWN panel programmatically.
    fn toggle_panel(&self, id: &str, open: Option<bool>);
    /// Dock `el` as a full-width strip in the shell's layout, between the chart area and
    /// the bottom bar (or under the bottom bar, see [`DockStripOptions`]): the charts
    /// shrink to make room. Strips stack in docking order. The caller owns `el`, its
    /// content and styling.
    fn dock_strip(&self, el: ElementHandle, opts: DockStripOptions) -> Box<dyn DockedStrip>;
    /// Dock `el` at the shell's outer right edge, full height: beside the chart column and
    /// any `column` panel, outside both (a rail of toggles). Edge elements stack left to
    /// right in docking order and keep their own width. Returns the undock.
    fn dock_edge(&self, el: ElementHandle, side: DockEdge) -> Unsubscribe;
    /// The widget's root element: pass it as `host` when mounting kit components
    /// (Dialog/Menu/Tooltip) from an action; without an explicit host they portal to the
    /// body, OUTSIDE the theme variables. A multi-chart shell hands its own root.
    fn host(&self) -> ElementHandle;
    /// The shell's translator: `text_of` for text an action or panel renders, `format()`
    /// for its numbers and dates, `direction()` for its layout. It follows `set_locale`;
    /// listen to the shell's `locale:changed` to re-render.
    fn i18n(&self) -> Rc<dyn Translator>;
    /// The widget's feedback pill (bottom-center, auto-hides).
    fn toast(&self, message: &str, kind: ToastKind);
    /// Add a SCRIPT indicator to the active chart THROUGH THE SHELL: unlike the raw
    /// `chart.add_indicator`, the addition enters the unified undo/redo timeline, the
    /// topbar indicator count, and the shell's bookkeeping. The entry's source rides the
    /// recorded action, so redo re-adds it even after the original handle died. The shell
    /// does NOT persist these across reloads (they are not manifest entries): a plugin
    /// that wants them back owns that via [`Contributions::register_state_persistence`].
    fn add_indicator(&self, entry: ExternalIndicatorEntry);
    /// Add a native (core-computed) indicator to the active chart through the shell:
    /// recorded in the undo/redo timeline, unlike the raw `chart.add_native_indicator`.
    fn add_native_indicator(&self, indicator_type: &str);
    /// Tell the shell some PERSISTABLE third-party state changed (a debounced
    /// `state:changed` + storage write follows). Only needed for state with no shell event
    /// of its own: indicator adds/removals already trigger the save cycle.
    fn state_changed(&self);
    /// The shell's built-in topbar items, shown or hidden at runtime: for a plugin that
    /// hosts one of those controls elsewhere (its own alerts surface, a rail of panel
    /// toggles).
    fn topbar(&self) -> &dyn TopbarItems;
    /// The shell's side-panel dock as data: which panels it holds (built-ins included),
    /// which one is open, and a change event.
    fn panels(&self) -> &dyn PanelDockView;
    /// The shell's right-edge toolbar (the `sidebar` option): badges on its entries, its
    /// entries hidden or shown at runtime, and a redraw after a sidebar action's `active`
    /// or `when` changed.
    fn sidebar(&self) -> &dyn SidebarControl;
    /// Where the right-click that opened a context menu landed. Set on the context handed
    /// to `context:*` actions (`when` and `run`); `None` everywhere else.
    fn pointer(&self) -> Option<ContextMenuPointer> {
        None
    }
    /// The implementing value as [`Any`], for a view layer that needs its own concrete
    /// context behind the trait object (the browser crate hands a context to script
    /// callbacks that act through the widget root). `None` unless the shell opts in.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }
}

// ── actions ────────────────────────────────────────────────

/// Where an action is projected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidgetActionTarget {
    Topbar,
    Sidebar,
    ContextBody,
    ContextPriceAxis,
    ContextTimeAxis,
}

impl WidgetActionTarget {
    /// The target's name as the original spells it (`context:price-axis`).
    pub fn as_str(self) -> &'static str {
        match self {
            WidgetActionTarget::Topbar => "topbar",
            WidgetActionTarget::Sidebar => "sidebar",
            WidgetActionTarget::ContextBody => "context:body",
            WidgetActionTarget::ContextPriceAxis => "context:price-axis",
            WidgetActionTarget::ContextTimeAxis => "context:time-axis",
        }
    }
}

/// Which cluster a topbar button joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionAlign {
    Left,
    Right,
}

/// Where a topbar action lands on the mobile layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobilePlacement {
    /// An icon stop on the bottom bar.
    Bar,
    /// A row in the three-dots menu.
    Menu,
}

pub type ActionGate = Rc<dyn Fn(&dyn WidgetContext) -> bool>;
pub type ActionRun = Rc<dyn Fn(&dyn WidgetContext)>;

#[derive(Clone)]
pub struct WidgetActionDescriptor {
    /// Stable id: re-registering an id replaces it.
    pub id: String,
    pub target: WidgetActionTarget,
    /// Always required, even icon-only: it is the button's aria-label and tooltip, the
    /// mobile drawer row's text, and the context-menu item's label. A message follows the
    /// shell's language; plain text shows as is.
    pub label: MessageLike,
    /// Icon id from the icon registry. The axis context menus carry no icons, so a row
    /// added there shows without it.
    pub icon: Option<String>,
    /// Topbar only: render the DESKTOP button icon-only, like the built-in tools: the
    /// `label` moves to the aria-label and a tooltip instead of button text (mobile
    /// surfaces keep their text). Requires `icon`: without one the flag is ignored (with a
    /// console warning) and the label renders.
    pub icon_only: bool,
    /// Sort key (ascending; default 0; ties keep registration order). On the topbar it
    /// orders the contributed group, and on the sidebar the `actions` flow slot. In a
    /// context menu (`context:*`) contributed rows sort TOGETHER with the menu's built-in
    /// rows, which sit at the ranks of `CONTEXT_MENU_BUILTIN_ORDER`.
    pub order: Option<f64>,
    /// Topbar only: which cluster the button joins. `Right` (default) is the right-hand
    /// tools cluster; `Left` puts it with the PRIMARY chrome buttons.
    pub align: Option<ActionAlign>,
    /// Topbar only: where the action goes on the MOBILE layout. Default: `Bar` for
    /// `align: Left`, `Menu` otherwise.
    pub mobile: Option<MobilePlacement>,
    /// Runtime gate; `None` means always shown.
    pub when: Option<ActionGate>,
    /// Sidebar only: the pressed look of a toggle that is on. Read whenever the sidebar
    /// redraws: call `ctx.sidebar().refresh()` after the state behind it changed.
    pub active: Option<ActionGate>,
    pub run: ActionRun,
}

impl WidgetActionDescriptor {
    /// An action with the original's defaults for everything optional.
    pub fn new(
        id: impl Into<String>,
        target: WidgetActionTarget,
        label: impl Into<MessageLike>,
        run: impl Fn(&dyn WidgetContext) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            target,
            label: label.into(),
            icon: None,
            icon_only: false,
            order: None,
            align: None,
            mobile: None,
            when: None,
            active: None,
            run: Rc::new(run),
        }
    }
}

/// Where a topbar action lands on the mobile layout (see [`WidgetActionDescriptor::mobile`]).
pub fn mobile_placement(
    align: Option<ActionAlign>,
    mobile: Option<MobilePlacement>,
) -> MobilePlacement {
    mobile.unwrap_or(if align == Some(ActionAlign::Left) {
        MobilePlacement::Bar
    } else {
        MobilePlacement::Menu
    })
}

/// An attachment's `mount`: runs once per widget, returns the disposer.
pub type AttachmentMount = Rc<dyn Fn(&dyn WidgetContext) -> Unsubscribe>;

/// A widget ATTACHMENT: a contributed unit of per-widget behavior/UI beyond a single
/// button: overlays, gesture handlers, custom key handling. `mount` runs once per widget
/// (at construction, or on `refresh_actions()` for late registrations) with the widget's
/// [`WidgetContext`]; the returned disposer runs at widget destroy. Everything the
/// attachment touches must come from `ctx` (never module globals).
#[derive(Clone)]
pub struct WidgetAttachment {
    /// Stable id: re-registering an id replaces it (mounted widgets keep the old one until
    /// destroy).
    pub id: String,
    pub mount: AttachmentMount,
}

impl WidgetAttachment {
    pub fn new(
        id: impl Into<String>,
        mount: impl Fn(&dyn WidgetContext) -> Unsubscribe + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            mount: Rc::new(mount),
        }
    }
}

// ── side panels ────────────────────────────────────────────

/// A contributed side panel's runtime handle: what `mount` hands back. Every member is
/// optional: a panel that only paints its body once needs none of them.
pub trait SidePanelHandle {
    /// (Re)bind to a chart instance: on mount, after every widget rebuild, and (in a
    /// workspace) whenever the active cell changes.
    fn on_chart(&self, _chart: &Vela) {}
    /// The panel just became visible. Panels that render lazily do it here.
    fn on_open(&self) {}
    /// Released when the panel is dropped (widget destroy, or a re-registration).
    fn destroy(&self) {}
}

/// The header surface a contributed panel may use: a SLOT between the title and the close
/// button for compact controls (a document name, action icons), and the title text itself.
/// Everything else in the header (the close button, the row) stays the shell's.
pub trait SidePanelHeader {
    /// Lay out inline controls here; the close button stays pinned right of it.
    fn slot(&self) -> ElementHandle;
    /// Replace the header title (an empty string hides it). The topbar toggle keeps the
    /// DECLARED `title` as its tooltip.
    fn set_title(&self, title: MessageLike);
    /// Whether the panel covers every chart right now (see `maximizable`).
    fn maximized(&self) -> bool;
    /// Maximize the panel over the shell's charts, or restore it: what its header button
    /// does. A no-op unless the descriptor is `maximizable`. Closing restores it too.
    fn set_maximized(&self, maximized: bool);
    /// Close the panel: what the header's close button does. A panel declared
    /// [`PanelHeaderMode::None`] wires its own close control to this.
    fn close(&self);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelHeaderMode {
    #[default]
    Default,
    /// Drops the shell's header row (title, header buttons, close) so the panel draws its
    /// own in its body. On the mobile layout the header's close is the panel's only way
    /// out, so a headless panel must provide one.
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelPlacement {
    /// A column in the chart row, beside the charts only.
    #[default]
    Chart,
    /// A full-height column at the shell's right, beside the topbar, the charts, the
    /// strips and the bottom bar, which all narrow to make room. On the mobile layout both
    /// kinds cover the chart area alike; maximized, a `Column` panel covers the chart
    /// column too.
    Column,
}

pub type PanelMount = Rc<
    dyn Fn(
        &dyn WidgetContext,
        &ElementHandle,
        &dyn SidePanelHeader,
    ) -> Option<Box<dyn SidePanelHandle>>,
>;

/// A contributed SIDE PANEL: a docked column in the shell's panel dock, alongside the
/// object tree and the data window, with a toggle button in the topbar's panel group.
///
/// The shell owns the chrome (header, close button, dock exclusivity, the button and its
/// pressed state) and hands `mount` the panel's BODY element to fill, plus a
/// [`SidePanelHeader`] for panels that dock controls in their header; the contribution
/// never reaches into the shell's DOM.
#[derive(Clone)]
pub struct SidePanelDescriptor {
    /// Stable id: re-registering an id replaces it. Also the key its width persists under.
    pub id: String,
    /// Header title, and the tooltip of its topbar button. A message follows the shell's
    /// language.
    pub title: MessageLike,
    /// Icon id from the icon registry.
    pub icon: String,
    /// Sort key among the panel buttons (ascending; default 100: after the built-ins).
    pub order: Option<f64>,
    /// Declared width in px (default 280).
    pub width: Option<f64>,
    /// Let the user drag the panel's inner edge (default false: a fixed column).
    pub resizable: bool,
    pub min_width: Option<f64>,
    pub max_width: Option<f64>,
    /// Float over the chart's right edge instead of docking beside it: the chart keeps its
    /// width and layout, and the panel covers its right-hand part (default false: a docked
    /// column that shrinks the chart). The dock stays exclusive either way.
    pub overlay: bool,
    /// A header button that maximizes the panel over every chart of the shell, and
    /// restores it (default false).
    pub maximizable: bool,
    pub header: PanelHeaderMode,
    pub placement: PanelPlacement,
    pub mount: PanelMount,
}

impl SidePanelDescriptor {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<MessageLike>,
        icon: impl Into<String>,
        mount: PanelMount,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            icon: icon.into(),
            order: None,
            width: None,
            resizable: false,
            min_width: None,
            max_width: None,
            overlay: false,
            maximizable: false,
            header: PanelHeaderMode::Default,
            placement: PanelPlacement::Chart,
            mount,
        }
    }
}

/// One panel toggle, as the shell's chrome consumes it (data, never DOM).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidePanelButton {
    pub id: String,
    pub title: String,
    pub icon: String,
}

/// Sort key of a panel that declares none: after the shell's own panels.
pub const DEFAULT_PANEL_ORDER: f64 = 100.0;

/// The built-in topbar slots a contributed action may TAKE OVER by registering under
/// their id: the "simple button" slots. The composites (symbol, timeframes, style, layout,
/// undo-redo, alerts, panels) are stateful controls the shell pushes state into; a plain
/// `{label, icon, run}` descriptor cannot stand in for them.
pub const OVERRIDABLE_TOPBAR_IDS: [&str; 2] = ["indicators", "screenshot"];

// ── legend actions and callouts ────────────────────────────

/// What a legend action sees about the indicator whose row it sits on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegendIndicatorInfo {
    pub id: String,
    pub title: String,
    /// The script source the indicator runs NOW (read at click time, so an in-place code
    /// update shows through); `None` for a NATIVE (core-computed) indicator. The usual
    /// `when` gate for source-centric actions.
    pub source: Option<String>,
    /// The language id of the engine running the script; `None` for a NATIVE indicator.
    /// The `when` gate for actions that only make sense for one language.
    pub language: Option<String>,
}

pub type LegendGate = Rc<dyn Fn(&LegendIndicatorInfo) -> bool>;
pub type LegendRun = Rc<dyn Fn(&dyn WidgetContext, &LegendIndicatorInfo)>;

/// A contributed LEGEND-ROW action: an icon button on every indicator's legend row,
/// revealed with the built-in controls (hover/selection), between them and the close
/// button. `when` gates per indicator (e.g. `source.is_some()` for actions that need the
/// script). `run` receives the shell's [`WidgetContext`] and the row's
/// [`LegendIndicatorInfo`].
#[derive(Clone)]
pub struct LegendActionDescriptor {
    /// Stable id: re-registering an id replaces it.
    pub id: String,
    /// Icon id from the icon registry.
    pub icon: String,
    /// Hover text. A message follows the chart's language.
    pub tooltip: MessageLike,
    /// Sort key within the contributed group (ascending; default 0).
    pub order: Option<f64>,
    /// Per-indicator gate; `None` means shown on every row.
    pub when: Option<LegendGate>,
    pub run: LegendRun,
}

/// One block of a legend callout's deployed panel, descriptor-side: plain text, or a
/// button whose `run` receives the shell's [`WidgetContext`] and the row's indicator: the
/// same signature as a legend action's `run`.
#[derive(Clone)]
pub enum LegendCalloutItem {
    Text {
        text: MessageLike,
    },
    Button {
        label: MessageLike,
        /// Emphasized (selection-colored) button: the panel's main action.
        primary: Option<bool>,
        /// Close the panel after `run` (default true).
        close: Option<bool>,
        run: LegendRun,
    },
}

/// The panel a clickable callout deploys: an optional heading over ordered blocks.
#[derive(Clone, Default)]
pub struct LegendCalloutContent {
    pub title: Option<MessageLike>,
    pub items: Vec<LegendCalloutItem>,
}

/// A callout's resolved presentation for one row.
#[derive(Clone)]
pub struct LegendCalloutSpec {
    /// Icon id from the icon registry.
    pub icon: String,
    /// Bubble fill: any CSS color (`color-mix` token washes match the built-in badges).
    pub background: String,
    /// Icon ink (default: the legend row's text color).
    pub color: Option<String>,
    /// Hover text; also the bubble's accessible name. A message follows the chart's
    /// language.
    pub tooltip: MessageLike,
    /// Deployed panel: presence makes the bubble clickable.
    pub content: Option<LegendCalloutContent>,
}

pub type CalloutResolve = Rc<dyn Fn(&LegendIndicatorInfo) -> Option<LegendCalloutSpec>>;

/// A contributed LEGEND CALLOUT: a small tinted bubble with a centered icon, visible right
/// of the indicator's legend title while the row is idle (hidden while its hover/selection
/// controls are out). When the spec carries `content`, clicking the bubble deploys that
/// panel.
///
/// Unlike a legend action's static icon, a callout's whole presentation is resolved per row
/// through `callout`: return `None` to show none (the per-indicator gate), or a spec whose
/// icon/tint/panel follow your own state (a market-status bubble changes dress as sessions
/// roll). Late state changes re-project through the shells' `refresh_actions()`, like every
/// contribution.
#[derive(Clone)]
pub struct LegendCalloutDescriptor {
    /// Stable id: re-registering an id replaces it.
    pub id: String,
    /// Sort key within the contributed group (ascending; default 0).
    pub order: Option<f64>,
    /// Resolve the row's bubble: `None` shows none.
    pub callout: CalloutResolve,
}

// ── state persistence ──────────────────────────────────────

/// The surface a `Cell`-scope persistence handler works against: bound to ONE cell (never
/// the active one by proxy), because serialize/restore run per cell, including cells that
/// are not active. `add_indicator`/`add_native_indicator` target THIS cell and are ALWAYS
/// muted: unlike their [`WidgetContext`] namesakes they never enter the undo timeline, even
/// from an async `restore` continuation (fetch, then add): applying a document is state
/// application, not a user edit.
pub trait CellStateContext {
    /// The cell's durable identity (`c1`, a declared name).
    fn cell_id(&self) -> String;
    /// The cell's LIVE chart.
    fn chart(&self) -> Vela;
    /// Add a script indicator to THIS cell through the shell.
    fn add_indicator(&self, entry: ExternalIndicatorEntry);
    /// Add a native indicator to THIS cell through the shell.
    fn add_native_indicator(&self, indicator_type: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateScope {
    /// Per chart (`charts[i].ext`): follows the cell through pool/layout moves.
    Cell,
    /// Document root (`state.ext`): one entry per document, whatever the grid.
    Global,
}

/// A third-party STATE PERSISTENCE handler: how a plugin puts its own state into the
/// shell's persisted document (the `ext` bag of the workspace / per-chart state) instead
/// of running a parallel store. `key` is namespaced (`vendor.feature`) and flat: one entry
/// per handler. `serialize` runs on every shell snapshot (`getState`, the persist write)
/// and returns a JSON-serializable payload, or `None` for "no entry". `restore` runs when
/// a document carrying the key is applied (boot restore, `applyState`), AFTER the core
/// state (chart alive, engines registered, indicator ledger converged) and, for cell
/// scope, inside the cell's history-mute, so nothing it does enters undo/redo. The payload
/// is UNTRUSTED (the codec passes `ext` through opaquely): validate it. `restore` is only
/// called for keys present in the document.
///
/// A key with no registered handler still round-trips verbatim, so a session without the
/// plugin never loses its state.
pub type CellSerialize = Rc<dyn Fn(&dyn CellStateContext) -> Option<Value>>;
pub type CellRestore = Rc<dyn Fn(&Value, &dyn CellStateContext)>;
pub type GlobalSerialize = Rc<dyn Fn(&dyn WidgetContext) -> Option<Value>>;
pub type GlobalRestore = Rc<dyn Fn(&Value, &dyn WidgetContext)>;

#[derive(Clone)]
pub enum StatePersistenceHandler {
    Cell {
        /// Namespaced entry key (`velapro.indicators`): re-registering a key replaces it.
        key: String,
        serialize: CellSerialize,
        restore: CellRestore,
    },
    Global {
        key: String,
        serialize: GlobalSerialize,
        restore: GlobalRestore,
    },
}

impl StatePersistenceHandler {
    pub fn key(&self) -> &str {
        match self {
            StatePersistenceHandler::Cell { key, .. }
            | StatePersistenceHandler::Global { key, .. } => key,
        }
    }

    pub fn scope(&self) -> StateScope {
        match self {
            StatePersistenceHandler::Cell { .. } => StateScope::Cell,
            StatePersistenceHandler::Global { .. } => StateScope::Global,
        }
    }
}

// ── symbol ranking ─────────────────────────────────────────

/// What a symbol ranking hands back: the reordered pool now, or when its future resolves.
pub enum RankedPool {
    Ready(Vec<SymbolDescriptor>),
    Pending(LocalBoxFuture<'static, Vec<SymbolDescriptor>>),
}

/// Reorder the symbol picker's AGGREGATED pool: every source combined, exactly what the
/// search dialog displays. Called when the pool changes (a provider's index lands or
/// refreshes), NOT per keystroke: the picker caches the result. May be async (a
/// server-fetched top list): the picker shows the current order and refreshes when the
/// future resolves.
///
/// The returned list may INJECT descriptors absent from the pool (they must carry their
/// `provider` and be genuinely servable, or selecting them parks the load) and may OMIT
/// entries (hiding them). The picker dedupes by venue+ticker, FIRST occurrence winning:
/// injecting at the head fixes both position and display data.
///
/// While a ranking is registered, the picker's built-in empty-query pin (the hardcoded
/// majors) stands down: the head of YOUR list is the dialog's opening screen. Under a
/// typed query the relevance tiers still lead: the ranking orders within each tier.
pub type SymbolRankingHook = Rc<dyn Fn(Vec<SymbolDescriptor>) -> RankedPool>;

// ── the registries ─────────────────────────────────────────

/// Registration order is kept; replacing an id keeps its place, as a `Map` does.
struct Keyed<T> {
    items: Vec<(String, Rc<T>)>,
}

impl<T> Default for Keyed<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> Keyed<T> {
    fn set(&mut self, id: &str, value: &Rc<T>) {
        match self.items.iter_mut().find(|(k, _)| k == id) {
            Some(slot) => slot.1 = Rc::clone(value),
            None => self.items.push((id.to_string(), Rc::clone(value))),
        }
    }

    fn remove(&mut self, id: &str) {
        self.items.retain(|(k, _)| k != id);
    }

    /// Remove `id` only while it still holds `value` (a stale disposer never removes a
    /// replacement).
    fn remove_if_same(&mut self, id: &str, value: &Rc<T>) {
        self.items
            .retain(|(k, v)| !(k == id && Rc::ptr_eq(v, value)));
    }

    fn values(&self) -> Vec<Rc<T>> {
        self.items.iter().map(|(_, v)| Rc::clone(v)).collect()
    }
}

#[derive(Default)]
struct Registries {
    actions: Keyed<WidgetActionDescriptor>,
    attachments: Keyed<WidgetAttachment>,
    panels: Keyed<SidePanelDescriptor>,
    legend_actions: Keyed<LegendActionDescriptor>,
    legend_callouts: Keyed<LegendCalloutDescriptor>,
    state_handlers: Keyed<StatePersistenceHandler>,
    symbol_ranking: Option<SymbolRankingHook>,
    default_engines: Vec<(String, EngineFactory)>,
}

/// Stable ascending sort by an optional key (`None` is `default`).
fn sort_by_order<T>(list: &mut [Rc<T>], order: impl Fn(&T) -> Option<f64>, default: f64) {
    list.sort_by(|a, b| {
        let (a, b) = (order(a).unwrap_or(default), order(b).unwrap_or(default));
        a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Every contribution registry a shell reads. Clones share the registries.
#[derive(Clone)]
pub struct Contributions {
    platform: Rc<dyn Platform>,
    reg: Rc<RefCell<Registries>>,
}

impl Contributions {
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self {
            platform,
            reg: Rc::new(RefCell::new(Registries::default())),
        }
    }

    fn disposer(&self, undo: impl FnOnce(&mut Registries) + 'static) -> Unsubscribe {
        let reg: Weak<RefCell<Registries>> = Rc::downgrade(&self.reg);
        Box::new(move || {
            if let Some(reg) = reg.upgrade() {
                undo(&mut reg.borrow_mut());
            }
        })
    }

    // ── widget actions ──

    /// Register (or replace) a widget action. Returns an unregister disposer. Widgets read
    /// the registry live. Registering under a RESERVED built-in topbar id (`indicators`,
    /// `screenshot`) OVERRIDES that slot (see [`topbar_action_override`]
    /// (Self::topbar_action_override)). Built-in ids that are stateful composites cannot be
    /// overridden and the registration is refused with a warning.
    pub fn register_widget_action(&self, desc: WidgetActionDescriptor) -> Unsubscribe {
        if TOPBAR_BUILTIN_IDS.contains(&desc.id.as_str())
            && !OVERRIDABLE_TOPBAR_IDS.contains(&desc.id.as_str())
        {
            self.platform.warn(&format!(
                "[vela] widget action \"{}\": this built-in topbar slot is a stateful control and cannot be overridden \u{2014} ignored. Overridable slots: {}.",
                desc.id,
                OVERRIDABLE_TOPBAR_IDS.join(", ")
            ));
            return Box::new(|| {});
        }
        let id = desc.id.clone();
        let rc = Rc::new(desc);
        self.reg.borrow_mut().actions.set(&id, &rc);
        self.disposer(move |r| r.actions.remove_if_same(&id, &rc))
    }

    pub fn unregister_widget_action(&self, id: &str) {
        self.reg.borrow_mut().actions.remove(id);
    }

    /// Actions for one target, `order`-sorted (ties keep registration order),
    /// `when`-filtered when a context is given.
    pub fn widget_actions(
        &self,
        target: WidgetActionTarget,
        ctx: Option<&dyn WidgetContext>,
    ) -> Vec<Rc<WidgetActionDescriptor>> {
        let mut list: Vec<Rc<WidgetActionDescriptor>> = self
            .reg
            .borrow()
            .actions
            .values()
            .into_iter()
            .filter(|d| d.target == target)
            .collect();
        sort_by_order(&mut list, |d| d.order, 0.0);
        match ctx {
            Some(ctx) => list
                .into_iter()
                .filter(|d| d.when.as_ref().is_none_or(|when| when(ctx)))
                .collect(),
            None => list,
        }
    }

    /// The action OVERRIDING a built-in topbar slot, if one is registered: an action whose
    /// `id` IS the built-in id. An override takes the slot's WHOLE surface: the desktop
    /// button (at the slot's position), the mobile counterpart, and the keyboard chord all
    /// route to `run`, and the native machinery behind the slot (the built-in indicator
    /// picker) is not constructed. Shells resolve this at construction.
    pub fn topbar_action_override(&self, id: &str) -> Option<Rc<WidgetActionDescriptor>> {
        if !OVERRIDABLE_TOPBAR_IDS.contains(&id) {
            return None;
        }
        self.reg
            .borrow()
            .actions
            .items
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| Rc::clone(v))
    }

    // ── attachments ──

    /// Register (or replace) a widget attachment. Returns an unregister disposer.
    pub fn register_widget_attachment(&self, att: WidgetAttachment) -> Unsubscribe {
        let id = att.id.clone();
        let rc = Rc::new(att);
        self.reg.borrow_mut().attachments.set(&id, &rc);
        self.disposer(move |r| r.attachments.remove_if_same(&id, &rc))
    }

    pub fn unregister_widget_attachment(&self, id: &str) {
        self.reg.borrow_mut().attachments.remove(id);
    }

    /// Every registered attachment (registration order).
    pub fn widget_attachments(&self) -> Vec<Rc<WidgetAttachment>> {
        self.reg.borrow().attachments.values()
    }

    // ── side panels ──

    /// Register (or replace) a side panel. Returns an unregister disposer.
    pub fn register_side_panel(&self, desc: SidePanelDescriptor) -> Unsubscribe {
        let id = desc.id.clone();
        let rc = Rc::new(desc);
        self.reg.borrow_mut().panels.set(&id, &rc);
        self.disposer(move |r| r.panels.remove_if_same(&id, &rc))
    }

    pub fn unregister_side_panel(&self, id: &str) {
        self.reg.borrow_mut().panels.remove(id);
    }

    /// Every registered side panel, `order`-sorted (registration order breaks ties).
    pub fn side_panels(&self) -> Vec<Rc<SidePanelDescriptor>> {
        let mut list = self.reg.borrow().panels.values();
        sort_by_order(&mut list, |d| d.order, DEFAULT_PANEL_ORDER);
        list
    }

    // ── legend actions ──

    /// Register (or replace) a legend action. Returns an unregister disposer.
    pub fn register_legend_action(&self, desc: LegendActionDescriptor) -> Unsubscribe {
        let id = desc.id.clone();
        let rc = Rc::new(desc);
        self.reg.borrow_mut().legend_actions.set(&id, &rc);
        self.disposer(move |r| r.legend_actions.remove_if_same(&id, &rc))
    }

    pub fn unregister_legend_action(&self, id: &str) {
        self.reg.borrow_mut().legend_actions.remove(id);
    }

    /// Every registered legend action, `order`-sorted (registration order breaks ties).
    pub fn legend_actions(&self) -> Vec<Rc<LegendActionDescriptor>> {
        let mut list = self.reg.borrow().legend_actions.values();
        sort_by_order(&mut list, |d| d.order, 0.0);
        list
    }

    // ── legend callouts ──

    /// Register (or replace) a legend callout. Returns an unregister disposer.
    pub fn register_legend_callout(&self, desc: LegendCalloutDescriptor) -> Unsubscribe {
        let id = desc.id.clone();
        let rc = Rc::new(desc);
        self.reg.borrow_mut().legend_callouts.set(&id, &rc);
        self.disposer(move |r| r.legend_callouts.remove_if_same(&id, &rc))
    }

    pub fn unregister_legend_callout(&self, id: &str) {
        self.reg.borrow_mut().legend_callouts.remove(id);
    }

    /// Every registered legend callout, `order`-sorted (registration order breaks ties).
    pub fn legend_callouts(&self) -> Vec<Rc<LegendCalloutDescriptor>> {
        let mut list = self.reg.borrow().legend_callouts.values();
        sort_by_order(&mut list, |d| d.order, 0.0);
        list
    }

    // ── state persistence ──

    /// Register (or replace) a state-persistence handler. Returns an unregister disposer.
    pub fn register_state_persistence(&self, handler: StatePersistenceHandler) -> Unsubscribe {
        let key = handler.key().to_string();
        let rc = Rc::new(handler);
        self.reg.borrow_mut().state_handlers.set(&key, &rc);
        self.disposer(move |r| r.state_handlers.remove_if_same(&key, &rc))
    }

    pub fn unregister_state_persistence(&self, key: &str) {
        self.reg.borrow_mut().state_handlers.remove(key);
    }

    /// The registered handlers of one scope (registration order).
    pub fn state_persistence_handlers(
        &self,
        scope: StateScope,
    ) -> Vec<Rc<StatePersistenceHandler>> {
        self.reg
            .borrow()
            .state_handlers
            .values()
            .into_iter()
            .filter(|h| h.scope() == scope)
            .collect()
    }

    // ── symbol ranking ──

    /// Register (or replace: ONE ranking at a time, last wins) the symbol ranking.
    /// Returns an unregister disposer.
    pub fn register_symbol_ranking(&self, hook: SymbolRankingHook) -> Unsubscribe {
        self.reg.borrow_mut().symbol_ranking = Some(Rc::clone(&hook));
        self.disposer(move |r| {
            if r.symbol_ranking
                .as_ref()
                .is_some_and(|h| Rc::ptr_eq(h, &hook))
            {
                r.symbol_ranking = None;
            }
        })
    }

    /// The registered ranking, if any: what the shells' symbol picker consults.
    pub fn symbol_ranking(&self) -> Option<SymbolRankingHook> {
        self.reg.borrow().symbol_ranking.clone()
    }

    // ── default scripting engines ──

    /// Register (or replace) a DEFAULT scripting engine for a language: every widget and
    /// workspace cell built afterwards registers `make()` on its chart automatically: the
    /// app-level wiring for hosts that pair Vela with an engine package, same shape as the
    /// other contribution registries. A per-instance `engines` option still wins for the
    /// same language, and the bare chart is untouched: with nothing registered here,
    /// nothing changes anywhere (there is still no bundled default engine).
    pub fn register_default_engine(&self, language: &str, make: EngineFactory) -> Unsubscribe {
        {
            let mut reg = self.reg.borrow_mut();
            match reg.default_engines.iter_mut().find(|(l, _)| l == language) {
                Some(slot) => slot.1 = Rc::clone(&make),
                None => reg
                    .default_engines
                    .push((language.to_string(), Rc::clone(&make))),
            }
        }
        let language = language.to_string();
        self.disposer(move |r| {
            r.default_engines
                .retain(|(l, f)| !(*l == language && Rc::ptr_eq(f, &make)));
        })
    }

    pub fn unregister_default_engine(&self, language: &str) {
        self.reg
            .borrow_mut()
            .default_engines
            .retain(|(l, _)| l != language);
    }

    /// The registered defaults merged UNDER `overrides`: per-instance factories win per
    /// language. The shell layers (widget, workspace cell) register exactly this result.
    pub fn resolve_engines(
        &self,
        overrides: Option<&HashMap<String, EngineFactory>>,
    ) -> Vec<(String, EngineFactory)> {
        let mut out = self.reg.borrow().default_engines.clone();
        if let Some(overrides) = overrides {
            // A stable order for the overrides that are new to the list.
            let mut extra: Vec<(&String, &EngineFactory)> = overrides.iter().collect();
            extra.sort_by(|a, b| a.0.cmp(b.0));
            for (language, make) in extra {
                match out.iter_mut().find(|(l, _)| l == language) {
                    Some(slot) => slot.1 = Rc::clone(make),
                    None => out.push((language.clone(), Rc::clone(make))),
                }
            }
        }
        out
    }
}

// ── legend providers ───────────────────────────────────────

/// A shell-side lookup of one indicator row on the chart.
pub type IndicatorLookup = Rc<dyn Fn(&str) -> Option<LegendIndicatorInfo>>;
/// A shell-side message resolver (`text_of` with the chart's translator).
pub type Translate = Rc<dyn Fn(&MessageLike) -> String>;
/// A fresh context per click.
pub type ContextFactory = Rc<dyn Fn() -> Rc<dyn WidgetContext>>;

/// The info of one indicator: natives carry neither `source` nor `language`.
fn legend_info_of(h: &dyn crate::engine::indicator_handle::IndicatorHandle) -> LegendIndicatorInfo {
    LegendIndicatorInfo {
        id: h.id().to_string(),
        title: h.title(),
        source: h.source(),
        language: h.language().map(str::to_string),
    }
}

/// The lookup and translator of one chart, held weakly so a provider stored in the chart's
/// own renderer never keeps the chart alive.
fn chart_lookup(chart: &Vela) -> (IndicatorLookup, Translate) {
    let weak = chart.downgrade();
    let find_weak = weak.clone();
    let find: IndicatorLookup = Rc::new(move |id| {
        let chart = find_weak.upgrade()?;
        let handle = chart.indicators().into_iter().find(|h| h.id() == id)?;
        Some(legend_info_of(handle.as_ref()))
    });
    let translate: Translate = Rc::new(move |m| match weak.upgrade() {
        Some(chart) => text_of(Some(&*chart.i18n()), m, &[]),
        None => text_of(None, m, &[]),
    });
    (find, translate)
}

impl Contributions {
    /// The provider a shell hands to `chart.renderer.set_legend_actions`: resolves the
    /// row's indicator on THAT chart, gates each descriptor, and binds `run` to a fresh
    /// context per click (never a cached one; the widget context rule). Both shells wire
    /// exactly this.
    pub fn legend_actions_provider_for(
        &self,
        chart: &Vela,
        context: ContextFactory,
    ) -> LegendActionProvider {
        let (find, translate) = chart_lookup(chart);
        self.legend_actions_provider(find, translate, context)
    }

    /// [`legend_actions_provider_for`](Self::legend_actions_provider_for) over an explicit
    /// indicator lookup and translator.
    pub fn legend_actions_provider(
        &self,
        find: IndicatorLookup,
        translate: Translate,
        context: ContextFactory,
    ) -> LegendActionProvider {
        let contributions = self.clone();
        Rc::new(move |indicator_id: &str| -> Vec<LegendActionView> {
            let Some(handle) = find(indicator_id) else {
                return Vec::new();
            };
            // `run` reads the indicator AGAIN at click time: rows are built once, and an
            // in-place code update since then must not hand the action the old source.
            contributions
                .legend_actions()
                .into_iter()
                .filter(|d| d.when.as_ref().is_none_or(|when| when(&handle)))
                .map(|d| {
                    let (find, context, run) =
                        (Rc::clone(&find), Rc::clone(&context), Rc::clone(&d.run));
                    let (id, fallback) = (indicator_id.to_string(), handle.clone());
                    LegendActionView {
                        id: d.id.clone(),
                        icon: d.icon.clone(),
                        tooltip: translate(&d.tooltip),
                        run: Rc::new(move || {
                            let info = find(&id).unwrap_or_else(|| fallback.clone());
                            let ctx = context();
                            run(ctx.as_ref(), &info);
                        }),
                    }
                })
                .collect()
        })
    }

    /// The provider a shell hands to `chart.renderer.set_legend_callouts`: resolves the
    /// row's indicator on THAT chart, lets each descriptor dress (or skip) the row, and
    /// binds every panel button to a fresh context per click (never a cached one).
    pub fn legend_callouts_provider_for(
        &self,
        chart: &Vela,
        context: ContextFactory,
    ) -> LegendCalloutProvider {
        let (find, translate) = chart_lookup(chart);
        self.legend_callouts_provider(find, translate, context)
    }

    /// [`legend_callouts_provider_for`](Self::legend_callouts_provider_for) over an
    /// explicit indicator lookup and translator.
    pub fn legend_callouts_provider(
        &self,
        find: IndicatorLookup,
        translate: Translate,
        context: ContextFactory,
    ) -> LegendCalloutProvider {
        let contributions = self.clone();
        Rc::new(move |indicator_id: &str| -> Vec<LegendCalloutView> {
            let Some(info) = find(indicator_id) else {
                return Vec::new();
            };
            let mut views = Vec::new();
            for d in contributions.legend_callouts() {
                let Some(spec) = (d.callout)(&info) else {
                    continue;
                };
                let content = spec.content.as_ref().map(|c| LegendCalloutPanel {
                    title: c.title.as_ref().map(|t| translate(t)),
                    items: c
                        .items
                        .iter()
                        .map(|item| match item {
                            LegendCalloutItem::Text { text } => LegendCalloutPanelItem::Text {
                                text: translate(text),
                            },
                            LegendCalloutItem::Button {
                                label,
                                primary,
                                close,
                                run,
                            } => {
                                let (run, context, info) =
                                    (Rc::clone(run), Rc::clone(&context), info.clone());
                                LegendCalloutPanelItem::Button {
                                    label: translate(label),
                                    primary: primary.unwrap_or(false),
                                    close: *close,
                                    run: Rc::new(move || {
                                        let ctx = context();
                                        run(ctx.as_ref(), &info);
                                    }),
                                }
                            }
                        })
                        .collect(),
                });
                views.push(LegendCalloutView {
                    id: d.id.clone(),
                    icon: spec.icon.clone(),
                    background: spec.background.clone(),
                    color: spec.color.clone(),
                    tooltip: translate(&spec.tooltip),
                    content,
                });
            }
            views
        })
    }
}

#[cfg(test)]
mod tests;
