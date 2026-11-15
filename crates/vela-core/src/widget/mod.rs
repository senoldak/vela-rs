//! The widget shell models, ported from the original `src/widget`.
//!
//! Everything here is platform-neutral: the shell's decisions as plain data and pure
//! functions. The DOM half of the original (element construction, CSS, listeners, timers)
//! belongs to the browser crate; each module's doc names what it left there.
//!
//! The groups, in the order of the original's `index.ts`:
//!
//! - options and defaults: [`shell_options`], [`shell_defaults`], [`persist`],
//!   [`layout_mode`], [`indicators`];
//! - compositions: [`topbar_composition`], [`sidebar_composition`];
//! - the contribution registries and the context contributions run against:
//!   [`contributions`];
//! - formatting and time: [`format`], [`timeframe`], [`timezones`], [`market_status`],
//!   [`session_shading`];
//! - models behind the chrome: [`statusline_model`], [`context_menu_model`],
//!   [`object_tree_model`], [`data_window`];
//! - pickers, bars, drawers and panels (the pure half of each component): [`topbar`],
//!   [`sidebar`], [`side_panel`], [`panel_dock`], [`statusline`], [`bottombar`],
//!   [`mobile_bar`], [`more_drawer`], [`drawings_drawer`], [`price_scale_drawer`],
//!   [`timeframe_drawer`], [`timeframe_quick`], [`timezone_drawer`], [`symbol_picker`],
//!   [`indicator_picker`], [`layout_picker`], [`object_tree`], [`cell_controls`],
//!   [`drawing_pill`], [`shortcuts_help`], [`toast`], [`watermark`], [`context_menu`];
//! - small helpers: [`history`], [`glide`], [`tool_shortcuts`], [`symbol_icon`].
//!
//! `VelaWidget` itself (the thin wrapper over the workspace) is not here.

pub mod bottombar;
pub mod cell_controls;
pub mod context_menu;
pub mod context_menu_model;
pub mod contributions;
pub mod data_window;
pub mod drawing_pill;
pub mod drawings_drawer;
pub mod format;
pub mod glide;
pub mod history;
pub mod indicator_picker;
pub mod indicators;
pub mod layout_mode;
pub mod layout_picker;
pub mod market_status;
pub mod mobile_bar;
pub mod more_drawer;
pub mod object_tree;
pub mod object_tree_model;
pub mod panel_dock;
pub mod persist;
pub mod price_scale_drawer;
pub mod session_shading;
pub mod shell_defaults;
pub mod shell_options;
pub mod shortcuts_help;
pub mod side_panel;
pub mod sidebar;
pub mod sidebar_composition;
pub mod statusline;
pub mod statusline_model;
pub mod symbol_icon;
pub mod symbol_picker;
pub mod timeframe;
pub mod timeframe_drawer;
pub mod timeframe_quick;
pub mod timezone_drawer;
pub mod timezones;
pub mod toast;
pub mod tool_shortcuts;
pub mod topbar;
pub mod topbar_composition;
pub mod watermark;

#[cfg(test)]
pub(crate) mod test_support;

// The export surface of the original's `index.ts`.
pub use bottombar::{RANGE_PRESETS, RangePreset};
pub use context_menu_model::CONTEXT_MENU_BUILTIN_ORDER;
pub use contributions::{
    CellStateContext, ContextMenuPointer, Contributions, DEFAULT_PANEL_ORDER,
    ExternalIndicatorEntry, OVERRIDABLE_TOPBAR_IDS, SidePanelButton, SidePanelDescriptor,
    SidePanelHandle, SidebarControl, StatePersistenceHandler, SymbolRankingHook,
    WidgetActionDescriptor, WidgetActionTarget, WidgetAttachment, WidgetContext,
};
pub use data_window::{DataWindowLine, DataWindowSection, data_window_sections};
pub use format::{decimals_for, fmt_change, fmt_price};
pub use indicator_picker::IndicatorRow;
pub use indicators::{
    IndicatorLoader, IndicatorManifest, IndicatorManifestEntry, ResolvedIndicator,
    resolve_indicators,
};
pub use persist::VelaStorage;
pub use shell_options::VelaShellOptions;
pub use side_panel::{
    DEFAULT_PANEL_MAX_WIDTH, DEFAULT_PANEL_MIN_WIDTH, DEFAULT_PANEL_WIDTH, SidePanelOptions,
    clamp_panel_width,
};
pub use sidebar_composition::{
    SIDEBAR_ACTIONS_SLOT, SIDEBAR_DEFAULT_BOTTOM, SIDEBAR_DEFAULT_TOP, SIDEBAR_PANELS_SLOT,
    SIDEBAR_SEPARATOR, SidebarComposition, SidebarLayout, SidebarSource, compose_sidebar,
};
pub use statusline::MarketStatus;
pub use statusline_model::StatuslinePart;
pub use symbol_picker::filter_symbols;
pub use timeframe::{ParsedTimeframe, parse_timeframe, timeframe_label, timeframe_ms};
pub use timezones::{TimezoneEntry, normalize_timezone, tz_button_label, tz_menu_label, tz_offset};
pub use topbar::price_style_label;
pub use topbar_composition::{
    ResolvedTopbarComposition, TOPBAR_BUILTIN_IDS, TOPBAR_DEFAULT_LEFT, TOPBAR_DEFAULT_RIGHT,
    TopbarComposition, pinned_topbar_action_ids, resolve_topbar_composition, topbar_has,
};
// The unified shell-state document: the SAME format the workspace exposes; the widget's
// `get_state` / `apply_state` speak it with a single `c1` cell.
pub use crate::state::{
    CellState, ChartState, DrawingToolsState, PanelsState, WorkspaceState, decode_state,
    encode_state, sanitize_state,
};
