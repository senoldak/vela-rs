//! The browser workspace and the widget root: the DOM skin of `VelaWorkspace.ts`,
//! `ChartCell.ts`, `splitters.ts`, `screenshot.ts`, `layout-mode.ts`, `toast.ts` and the
//! storage adapter, plus the mount points the shell components plug into.
//!
//! The logic is `vela_core::workspace` (the [`Workspace`](vela_core::workspace::Workspace))
//! and `vela_core::widget` (the models of each component). This module puts a workspace on
//! a page:
//!
//! ```text
//! JS: VelaWorkspace / VelaWidget          (api.rs)
//!  └─ WidgetRoot                          (root.rs)   the DOM skeleton and the shared services
//!  └─ BrowserWorkspaceHost                (host.rs)   WorkspaceHost: cells, grid, toast, screenshot
//!  └─ Workspace                           (vela-core)
//! ```
//!
//! Nothing here fetches anything: the default data is an in-memory provider
//! ([`MemoryProvider`], seeded from JavaScript). The exchange providers are opt-in
//! (`providers: ['binance' | 'coinbase' | 'hyperliquid']`).
//!
//! # DOM
//!
//! ```text
//! div.vela-workspace.vela-ui [data-vela-regions] [data-layout] [lang] [dir]   --vela-* tokens
//! ├─ div.vela-widget-topbar.vela-ws-region      [data-vela-region=topbar]      grid area 1 / 1-2
//! ├─ div.vela-ws-toolbar.vela-ws-region         [toolbar]                      grid area 2-5 / 1
//! ├─ div.vela-ws-main                           [main]                         grid area 2 / 2
//! │  └─ div.vela-ws-grid [data-multi] [data-maximized]
//! │     ├─ div.vela-cell [data-cell-id] [data-active] [data-drop-target]   one per live cell
//! │     │  ├─ div.vela-native           the chart's wrapper (BrowserHost)
//! │     │  └─ a.vela-attribution        in the bottom-left cell only
//! │     ├─ div.vela-ws-splitter [data-axis] [data-index]                   the strips
//! │     └─ div.vela-toast [data-open] [data-type]
//! ├─ div.vela-ws-strips.vela-ws-region          [strips]                       grid area 3 / 2
//! ├─ div.vela-widget-bottombar.vela-ws-region   [bottombar]                    grid area 4 / 2
//! ├─ div.vela-ws-strips.vela-ws-strips-below    [strips-below]                 grid area 5 / 2
//! ├─ div.vela-mobilebar.vela-ws-region          [mobilebar]                    grid area 6 / 1-end
//! ├─ div.vela-ws-column.vela-ws-region          [column]                       grid area 1-5 / 3
//! └─ div.vela-ws-edge.vela-ws-region            [edge]                         grid area 1-5 / 4
//! ```
//!
//! Every region except `main` is EMPTY until a component mounts into it, and an empty
//! region is `display: none`: an empty shell is the workspace grid alone, filling the root.
//! The class names and the grid areas are the original's (`VelaWorkspace.ts`'s stylesheet),
//! so a component's CSS written against the original applies unchanged.
//!
//! # The contract for shell components
//!
//! A component (topbar, sidebar, side panels and their dock, bottom bar, statusline,
//! watermark, cell controls, drawers, pickers, the object tree...) is written against
//! [`WidgetRoot`]. It receives an `Rc<WidgetRoot>` and does four things.
//!
//! **1. Mount.** `root.mount(Region::Topbar, &el)` appends `el` to the region and returns a
//! [`Mount`] guard that removes it when dropped. The component builds its own element tree
//! and CSS (inject one `<style id=...>` per document, in the `vela.chrome` layer, reading
//! the tokens). Per-cell overlays mount into `root.cell_container(id)` (the
//! `div.vela-cell`: statusline, watermark, cell controls, the context menu). Floating
//! layers (menus, drawers, pickers) append to `root.element()`.
//!
//! **2. Read the workspace.** `root.workspace()` is `Some(Workspace)` once the workspace
//! exists, which it does before any component mounts. Keep `root.weak_workspace()` in
//! closures. The active cell is `ws.active()`; the projection of the shared chrome is
//! `ws.projection()`, re-read on [`WorkspaceEvent::Projection`](vela_core::workspace::WorkspaceEvent).
//! Subscribe with `ws.on(WorkspaceEventName::..., handler)` and keep the returned
//! subscription; `ws.off(sub)` in your disposer. Handlers run after the operation, outside
//! any borrow: they may call back into the workspace.
//!
//! **3. Use the shared services** (all on [`WidgetRoot`]):
//!
//! | Service | Call |
//! |---------|------|
//! | translator | `root.translator()` (the workspace's; re-ask after `locale:changed`) |
//! | theme | `root.theme()`, `root.on_theme(cb)`; the colors as `var(--vela-*)` ([`tokens`]) |
//! | layout mode | `root.layout_mode()`, `root.on_layout_mode(cb)` (`data-layout` on the root) |
//! | storage | `root.storage()`: [`LocalStorageAdapter`], both storage traits |
//! | keymap | `root.keymap()`: register `KeyBindingDescriptor`s; the root feeds it `keydown` |
//! | toast | `root.toast(message, kind, ms)` (the pill on the grid) |
//! | shell options | `root.shell_options()` (what the widget was created with) |
//! | platform / env | `root.platform()`, `root.env()` |
//! | deferral | `root.defer(f)`: run `f` in a microtask |
//! | cleanup | `root.on_dispose(f)`: runs when the root is destroyed, in reverse order |
//!
//! **4. Persist.** State that belongs to the document goes through the workspace
//! (`ws.set_panels_state`, `ws.set_timeframe_favorite`, `ws.mark_state_dirty()` after a
//! change of persisted state); the workspace debounces and writes it. `root.storage()` is for
//! what is not part of the document.
//!
//! What a component must not do: call the [`Workspace`](vela_core::workspace::Workspace)
//! from inside a [`WorkspaceHost`](vela_core::workspace::WorkspaceHost) method (components
//! are not host methods, so this only concerns code that runs from `present_grid`,
//! `create_cell`, ...: it does not exist outside this module), hold a `Workspace` strongly
//! in a closure it hands to the root (use the weak handle), or leave DOM behind after the
//! root's `destroy` (use [`WidgetRoot::on_dispose`]).
//!
//! # Not here
//!
//! The shell components themselves (`shell_top`, `shell_side`); the original's
//! `createToaster` stack (the status pill is the only toast surface); drag handles of a cell
//! to swap slots (the API is `Workspace::swap_cells`, `set_drop_target`).
//!
//! # Shared by the whole shell
//!
//! - [`WidgetRoot::contributions`]: the ONE registry of contributed actions, panels,
//!   attachments, legend actions and callouts and the symbol ranking, filled by the JavaScript
//!   `register*` functions ([`registry`]). The topbar, the sidebar, the dock, the context
//!   menu, the mobile menu and every cell's legend read it; [`WidgetRoot::refresh_actions`]
//!   re-projects after a late registration.
//! - [`WidgetRoot::context`]: the ONE `WidgetContext` ([`RootContext`]). The parts fill its
//!   slots ([`WidgetRoot::slots`]) when they mount: the dock, the sidebar, the topbar's item
//!   visibility, the symbol search.
//! - The chart dialogs (settings, indicator inputs) open at the root, as the original's
//!   `dialogHost: root` does; the drawing settings panel stays in its cell, as it does there.

pub mod api;
pub mod context;
pub mod css;
pub mod host;
pub mod js;
pub mod memory_provider;
pub mod registry;
pub mod root;
pub mod splitters;
pub mod storage;
pub mod toast;
pub mod tokens;

pub use api::{VelaWidget, VelaWorkspace, VelaWorkspaceCell};
pub use context::{ContextSlots, RootContext};
pub use host::BrowserWorkspaceHost;
pub use memory_provider::{MemoryProvider, SymbolSeries};
pub use registry::{context_object, global_contributions, global_shell_defaults};
pub use root::{Mount, Region, RootOptions, WidgetRoot};
pub use storage::LocalStorageAdapter;
pub use toast::ToastView;
