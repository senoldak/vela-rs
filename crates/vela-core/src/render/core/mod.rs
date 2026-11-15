//! The renderer's core math, ported from the original `native/core`: the
//! coordinate system and viewport, autoscale and manual price scaling, pane
//! resize and layer stacking, frame scheduling and animation timing, the
//! retained scene graph, and the persisted chart configuration.
//!
//! Everything here is platform-neutral. What stays with the browser crate,
//! file by file:
//!
//! - `Scheduler.ts` / `Animator.ts`: `requestAnimationFrame`,
//!   `cancelAnimationFrame` and `performance.now()` are passed in as callbacks
//!   ([`scheduler::RequestFrame`], [`scheduler::CancelFrame`],
//!   [`animator::Now`]); the browser crate supplies the real ones.
//! - `plotDirection.ts`: `pinPlotCanvas` (it sets `canvas.dir` on an
//!   `HTMLCanvasElement`): see [`plot_direction`].
//! - `InputController.ts` / `KeyboardController.ts`: the DOM listeners, pointer
//!   capture, long-press timers, cursor styles and the press state machine.
//!   Their pure decision logic is in [`input_logic`].
//! - `SceneGraph.ts`: a drawing slice's canvas is the opaque
//!   [`CanvasImage`](crate::render::canvas::CanvasImage) handle.
//! - `chartConfig.ts`: the chart-type registry it reads is a
//!   [`ChartTypeRegistry`](crate::chart_types::ChartTypeRegistry) argument.
//!
//! [`color`] holds the two color helpers the configuration needs from the
//! original's UI kit (`parseRgb`, `withAlpha`); they are not exported for other
//! modules' use.

pub mod animator;
pub mod autoscale;
pub mod bar_time;
pub mod chart_config;
pub mod color;
pub mod coordinate_system;
pub mod input_logic;
pub mod layer_stacking;
pub mod manual_scale;
pub mod overlay_stack;
pub mod pane_resize;
pub mod plot_direction;
pub mod scene_graph;
pub mod scheduler;
pub mod viewport_state;

pub use animator::{Animator, EaseSetting, ease_toward};
pub use coordinate_system::{CoordinateSystem, PaneBounds, PriceScale};
pub use scene_graph::{PaneNode, SceneGraph};
pub use scheduler::{InvalidateLevel, Scheduler, repaints_chrome, repaints_data};
pub use viewport_state::{
    MAX_BAR_SPACING, MIN_BAR_SPACING, ViewportState, clamp_bar_spacing, default_viewport,
};
