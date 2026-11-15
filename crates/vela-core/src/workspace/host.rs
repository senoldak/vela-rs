//! The host seam of the workspace: everything the original `VelaWorkspace` and `ChartCell`
//! read from or do to the DOM, reduced to one small trait.
//!
//! `vela-core` has no DOM, so the workspace is written against [`WorkspaceHost`] and the
//! browser crate implements it, the same way [`NativeHost`] serves the renderer shell. The
//! reference implementation is [`TestWorkspaceHost`](super::testing::TestWorkspaceHost).
//!
//! The workspace host is a FACTORY of [`NativeHost`]s: it creates a container per cell and
//! hands back the host the cell's chart mounts on. It also receives the grid's
//! presentation as data ([`GridPresentation`]) after every structural change.
//!
//! The same ownership rules as the renderer shell apply. The workspace's state sits
//! behind one `RefCell`; events are delivered after the borrow is released; but the host
//! is called while the workspace is mid-operation, so a host must not call back into the
//! workspace from inside any of these methods (queue the work and run it from the next
//! event or frame). Subscribers to workspace events are not affected: they run after the
//! operation and may call back in freely.
//!
//! What a browser does per member is listed on each method.

use std::rc::Rc;

use crate::render::canvas::Canvas2d;
use crate::render::native::{NativeHost, NativeRenderer};

use super::cell::ChartCell;
use super::screenshot::{CellBox, GridBox};

/// How a toast reads (`'info' | 'success' | 'error'`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl ToastKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// One cell's place in the grid, for the host to reflect.
#[derive(Clone, Debug, PartialEq)]
pub struct CellPlacement {
    /// The cell's durable identity (`btc`, `c3`).
    pub id: String,
    /// The slot position, 0-based (DOM order of an auto-flow grid).
    pub slot: usize,
    /// The slot id of the layout (`c1`..`cN`): pure geometry.
    pub slot_id: String,
    /// The `grid-area` the cell takes: the named area of an asymmetric layout, or the
    /// full-grid span (`"1 / 1 / -1 / -1"`) while a cell is maximized; `None` auto-flows.
    pub grid_area: Option<String>,
    /// `true`: the cell stays alive but invisible beneath the maximized one
    /// (`visibility: hidden`).
    pub hidden: bool,
    /// The maximized cell sits on top (`z-index: 5`); everything else is `None`.
    pub z_index: Option<i32>,
    /// This is the active cell (`data-active`): the host paints the active ring, only on
    /// multi-cell, non-maximized grids ([`GridPresentation::multi`]).
    pub active: bool,
    /// This cell is the live drop target of a grip drag (`data-drop-target`).
    pub drop_target: bool,
}

/// The grid container's presentation: what `applyGrid` writes in the original.
#[derive(Clone, Debug, PartialEq)]
pub struct GridPresentation {
    /// The layout id (`"4"`, `"g3x2"`).
    pub layout: String,
    /// Column and row track weights in force (`fr`), splitter overrides included.
    pub cols: Vec<f64>,
    pub rows: Vec<f64>,
    /// `grid-template-areas` rows of an asymmetric layout.
    pub areas: Option<Vec<String>>,
    /// The grid holds more than one cell (`data-multi`): gates the active ring.
    pub multi: bool,
    /// A cell is maximized over the grid (`data-maximized`): the splitter strips and the
    /// ring hide.
    pub maximized: Option<String>,
    /// Live cells in slot order.
    pub cells: Vec<CellPlacement>,
    /// The splitter strips need re-laying out against these tracks (recompute with
    /// [`splitter_strips`](super::splitter_strips) once the container size is known).
    pub gap_px: f64,
}

/// A surface the host created to composite a layout screenshot (a canvas the size of the
/// grid). Paint on [`context`](Self::context), then [`encode_png`](Self::encode_png).
pub trait ScreenshotSurface {
    /// The 2D context to paint on.
    fn context(&self) -> Rc<dyn Canvas2d>;
    /// The painted image as a PNG data URL.
    fn encode_png(&self) -> Option<String>;
    /// Rasterize cell `cell_id`'s DOM overlays (legends, status line, watermark, attribution
    /// mark) into its tile at `dest`: `Under` runs before the tile's canvases, `Over` after
    /// them. A surface without a DOM draws nothing.
    fn paint_overlays(
        &self,
        _cell_id: &str,
        _phase: super::screenshot::OverlayPhase,
        _dest: &super::screenshot::DestRect,
    ) {
    }
}

/// The host of a workspace. See the module documentation.
pub trait WorkspaceHost {
    // ── cells ──

    /// Create the container of cell `id` inside the grid (a `div.vela-cell`, relative,
    /// overflow clipped, `data-cell-id`) and return the host its chart mounts on (a
    /// `NativeHost` over that container). Called when a layout creates or restores a
    /// cell. A browser also wires the container's capture-phase `pointerdown` and
    /// `focusin` to [`Workspace::activate`](super::Workspace::activate)`(id)`.
    fn create_cell(&self, id: &str) -> Rc<dyn NativeHost>;

    /// The cell's chart and renderer exist: bind the DOM that needs the renderer
    /// (gestures, the legend, dialogs, the statusline and watermark overlays). Called
    /// once per cell right after construction, before the cell loads its first bars.
    /// The renderer is the chart's own (`chart.renderer()` is the facade over it).
    fn cell_mounted(&self, _id: &str, _renderer: &Rc<NativeRenderer>) {}

    /// Right after [`cell_mounted`](Self::cell_mounted): the cell itself, whose chart exists
    /// (`cell.try_chart()`). Bind what needs the engine here (translators, dialog sources,
    /// the chart's events): the cell is handed over so no microtask is needed. Only the
    /// cell's own chart may be called; the workspace may not (it is mid-operation).
    fn cell_ready(&self, _id: &str, _cell: &ChartCell) {}

    /// Remove the cell's container (the chart is already destroyed). Called on every
    /// layout change that drops or rebuilds a cell, on a state rebuild and on destroy.
    fn destroy_cell(&self, id: &str);

    // ── grid presentation ──

    /// The grid changed (layout, track weights, slot order, active cell, maximize, drop
    /// target): write the template and each cell's placement, move the cells' containers
    /// into slot order, and re-lay out the splitter strips.
    fn present_grid(&self, _grid: &GridPresentation) {}

    // ── effects ──

    /// A notice for the user (unresolved symbols, script errors, alerts). Default: drop it.
    fn toast(&self, _message: &str, _kind: ToastKind, _duration_ms: f64) {}

    // ── screenshot ──

    /// The grid's viewport box, its device pixel ratio and computed background color (the
    /// seam color), or `None` when it is not laid out.
    fn grid_metrics(&self) -> Option<(GridBox, f64, String)> {
        None
    }

    /// Cell `id`'s viewport box (`hidden`: a maximize hides it).
    fn cell_box(&self, _id: &str) -> Option<CellBox> {
        None
    }

    /// A compositing surface of `width` x `height` DEVICE pixels, or `None` when the host
    /// cannot paint one.
    fn screenshot_surface(&self, _width: f64, _height: f64) -> Option<Rc<dyn ScreenshotSurface>> {
        None
    }
}
