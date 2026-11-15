//! The multi-chart workspace, ported from the original `src/workspace`: a grid of
//! [`ChartCell`]s with stable identities behind one shared data feed, an active cell,
//! resizable splitters, and a layout registry ([`LayoutRegistry`]) plugins extend like every
//! other Vela registry.
//!
//! `ChartCell` and `VelaWorkspace` are mostly logic with a thin DOM skin. The logic is
//! platform neutral here, written against one small host seam ([`WorkspaceHost`]) the
//! browser crate implements, the same way the renderer shell is written against
//! [`NativeHost`](crate::render::native::NativeHost). The in-memory implementation is
//! [`testing::TestWorkspaceHost`].
//!
//! # Files
//!
//! | Original            | Here                                                          |
//! |---------------------|---------------------------------------------------------------|
//! | `layouts.ts`        | [`layouts`]: the registry, grid math, glyph, reducers          |
//! | `splitters.ts`      | [`splitters`]: track math, seam segments, strips, drag         |
//! | `sync.ts`           | [`sync`]: targets, groups, drawings pairing, style patch       |
//! | `persist.ts`        | [`persist`]: the storage trait and the memory adapter          |
//! | `context.ts`        | [`context`]: the live widget context of the workspace          |
//! | `screenshot.ts`     | [`screenshot`]: the compositing plan and the blit              |
//! | `WorkspaceReplay.ts`| [`replay`]                                                     |
//! | `ChartCell.ts`      | [`cell`]                                                       |
//! | `VelaWorkspace.ts`  | [`shell`]                                                      |
//! | `index.ts`          | the re-exports below                                           |
//!
//! The cell reuses the widget port's pure models where they exist: `crate::widget::timeframe`
//! (the timeframe grammar), `crate::widget::history` (the undo timeline, re-exported here as
//! [`CellHistory`]) and `crate::widget::indicators::ResolvedIndicator` (the manifest entry,
//! [`ManifestIndicator`]).

pub mod cell;
pub mod context;
pub mod host;
pub mod layouts;
pub mod persist;
pub mod replay;
#[cfg(test)]
mod rig;
pub mod screenshot;
pub mod shell;
pub mod splitters;
pub mod sync;
pub mod testing;

pub use crate::widget::history::{HistoryAction, WidgetHistory as CellHistory};
pub use cell::{
    CellBoot, CellChartDefaults, CellDeps, CellExtHandler, CellNativeInfo, CellObserver,
    CellRangePreset, CellSeed, CellStatusPrefs, ChartCell, EngineFactories, ExternalIndicatorEntry,
    LedgerInputs, LibraryRow, ManifestAddOptions, ManifestIndicator, OnChartRow, PooledCellState,
    StatuslinePart, StatuslinePartsOn, WeakChartCell, cell_chart_defaults, cell_drawings,
    indicator_ledger, ledger_entry_name, ledger_native_type, seed_defaults,
};
pub use context::{ContextCell, WorkspaceWidgetContext};
pub use host::{CellPlacement, GridPresentation, ScreenshotSurface, ToastKind, WorkspaceHost};
pub use layouts::{
    GRID_PICKER_MAX, GridStyles, LayoutCell, LayoutDefinition, LayoutRegistration, LayoutRegistry,
    LayoutShape, LayoutTrackSizes, active_after_layout, grid_styles, layout_glyph, layout_shape,
    occupancy_grid, order_after_layout, svg16,
};
pub use persist::{
    MemoryStorage, StorageRead, WorkspaceStorage, decode_state, encode_state,
    memory_storage_adapter, sanitize_state,
};
pub use replay::{
    ReplayCell, WorkspaceReplay, WorkspaceReplayEvent, WorkspaceReplayEventName,
    WorkspaceReplayHost, WorkspaceReplayStartOptions, bar_close, last_open_closed_by,
};
pub use screenshot::{
    CellBox, DestRect, GridBox, LayoutShotFrame, LayoutShotTile, ShotRect,
    composite_layout_screenshot, dest_rect, tiles_from_cell_rects,
};
pub use shell::{
    ActiveProjection, DEFAULT_PERSIST_KEY, GlobalExtHandler, LayoutChoice, SyncRow, TypingState,
    TypingTarget, WeakWorkspace, Workspace, WorkspaceAlert, WorkspaceError, WorkspaceEvent,
    WorkspaceEventName, WorkspaceOptions, WorkspaceScriptRun, WorkspaceSubscription,
    declared_order, next_auto_cell_id, resolve_typing,
};
pub use splitters::{
    Axis, SplitterDrag, SplitterStrip, even_tracks, resize_tracks, seam_segments, segment_span_px,
    splitter_strips, track_offsets,
};
pub use sync::{
    CellDrawings, STYLE_SYNC_CONFIG_KEYS, SyncKind, SyncOptions, SyncSetting, drawing_link_content,
    pair_identical_drawings, ranges_within, style_config_patch, sync_groups, sync_targets,
};
