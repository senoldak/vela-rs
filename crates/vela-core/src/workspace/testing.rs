//! An in-memory [`WorkspaceHost`] for tests, and the reference implementation of the seam
//! for the browser crate's author.
//!
//! Every cell gets its own [`TestHost`] (the renderer shell's in-memory host: recording
//! canvases, hand-stepped frames and clocks), made on demand by
//! [`WorkspaceHost::create_cell`]. Everything the workspace pushes (the grid
//! presentations, toasts, mounted renderers, created and destroyed containers) is recorded
//! and readable, and the screenshot geometry is whatever the test sets.
//!
//! ```ignore
//! let host = TestWorkspaceHost::new(800.0, 400.0);
//! let ws = Workspace::new(host.clone(), env, options)?;
//! host.run_until_idle();
//! assert_eq!(host.created(), ["a", "b", "c", "d"]);
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::render::canvas::{Canvas2d, RecordingCanvas};
use crate::render::native::testing::TestHost;
use crate::render::native::{NativeHost, NativeRenderer};

use super::cell::ChartCell;
use super::host::{GridPresentation, ScreenshotSurface, ToastKind, WorkspaceHost};
use super::screenshot::{CellBox, DestRect, GridBox, OverlayPhase};

/// A [`ScreenshotSurface`] that records what is painted on it.
pub struct TestSurface {
    canvas: Rc<RecordingCanvas>,
    size: (f64, f64),
    overlays: RefCell<Vec<(String, OverlayPhase)>>,
}

impl TestSurface {
    pub fn recording(&self) -> Rc<RecordingCanvas> {
        self.canvas.clone()
    }

    /// The overlay rasterizations the workspace asked for: cell id and phase, in order.
    pub fn overlays(&self) -> Vec<(String, OverlayPhase)> {
        self.overlays.borrow().clone()
    }

    /// The backing size the workspace asked for.
    pub fn size(&self) -> (f64, f64) {
        self.size
    }
}

impl ScreenshotSurface for TestSurface {
    fn context(&self) -> Rc<dyn Canvas2d> {
        self.canvas.clone()
    }

    fn encode_png(&self) -> Option<String> {
        Some("data:image/png;base64,LAYOUT".to_string())
    }

    fn paint_overlays(&self, cell_id: &str, phase: OverlayPhase, _dest: &DestRect) {
        self.overlays
            .borrow_mut()
            .push((cell_id.to_string(), phase));
    }
}

/// The in-memory workspace host. See the module documentation.
pub struct TestWorkspaceHost {
    cell_size: Cell<(f64, f64)>,
    dpr: Cell<f64>,
    hosts: RefCell<Vec<(String, Rc<TestHost>)>>,
    created: RefCell<Vec<String>>,
    destroyed: RefCell<Vec<String>>,
    mounted: RefCell<Vec<String>>,
    ready: RefCell<Vec<(String, bool)>>,
    grids: RefCell<Vec<GridPresentation>>,
    toasts: RefCell<Vec<(String, ToastKind)>>,
    grid_box: Cell<Option<GridBox>>,
    cell_boxes: RefCell<HashMap<String, CellBox>>,
    surfaces: RefCell<Vec<Rc<TestSurface>>>,
}

impl TestWorkspaceHost {
    /// A host whose cells are `cell_width` x `cell_height` CSS px.
    pub fn new(cell_width: f64, cell_height: f64) -> Rc<Self> {
        Rc::new(Self {
            cell_size: Cell::new((cell_width, cell_height)),
            dpr: Cell::new(1.0),
            hosts: RefCell::default(),
            created: RefCell::default(),
            destroyed: RefCell::default(),
            mounted: RefCell::default(),
            ready: RefCell::default(),
            grids: RefCell::default(),
            toasts: RefCell::default(),
            grid_box: Cell::new(None),
            cell_boxes: RefCell::default(),
            surfaces: RefCell::default(),
        })
    }

    /// The live [`TestHost`] of cell `id` (the latest one when a cell was rebuilt), or
    /// `None` when the cell has none (never created, or destroyed).
    pub fn cell_host(&self, id: &str) -> Option<Rc<TestHost>> {
        self.hosts
            .borrow()
            .iter()
            .rev()
            .find(|(k, _)| k == id)
            .map(|(_, h)| h.clone())
    }

    /// Step every live cell host's frames until idle.
    pub fn run_until_idle(&self) {
        let hosts: Vec<Rc<TestHost>> = self.hosts.borrow().iter().map(|(_, h)| h.clone()).collect();
        for h in hosts {
            h.run_until_idle();
        }
    }

    /// Every container created, in order (a rebuilt cell appears again).
    pub fn created(&self) -> Vec<String> {
        self.created.borrow().clone()
    }

    /// Every container destroyed, in order.
    pub fn destroyed(&self) -> Vec<String> {
        self.destroyed.borrow().clone()
    }

    /// The ids of the cells whose renderer was handed to `cell_mounted`, in order.
    pub fn mounted(&self) -> Vec<String> {
        self.mounted.borrow().clone()
    }

    /// The cells handed to `cell_ready`, in order, with whether their chart existed then.
    pub fn ready(&self) -> Vec<(String, bool)> {
        self.ready.borrow().clone()
    }

    /// Every grid presentation pushed, oldest first.
    pub fn grids(&self) -> Vec<GridPresentation> {
        self.grids.borrow().clone()
    }

    /// The last grid presentation pushed.
    pub fn last_grid(&self) -> Option<GridPresentation> {
        self.grids.borrow().last().cloned()
    }

    /// Every toast, oldest first.
    pub fn toasts(&self) -> Vec<(String, ToastKind)> {
        self.toasts.borrow().clone()
    }

    /// Set the geometry [`WorkspaceHost::grid_metrics`] reports.
    pub fn set_grid(&self, grid: GridBox, dpr: f64) {
        self.grid_box.set(Some(grid));
        self.dpr.set(dpr);
    }

    /// Set the box [`WorkspaceHost::cell_box`] reports for cell `id`.
    pub fn set_cell_box(&self, id: &str, cell_box: CellBox) {
        self.cell_boxes
            .borrow_mut()
            .insert(id.to_string(), cell_box);
    }

    /// The compositing surfaces handed out, oldest first.
    pub fn surfaces(&self) -> Vec<Rc<TestSurface>> {
        self.surfaces.borrow().clone()
    }
}

impl WorkspaceHost for TestWorkspaceHost {
    fn create_cell(&self, id: &str) -> Rc<dyn NativeHost> {
        let (w, h) = self.cell_size.get();
        let host = TestHost::new(w, h, 1.0);
        self.hosts.borrow_mut().push((id.to_string(), host.clone()));
        self.created.borrow_mut().push(id.to_string());
        host
    }

    fn cell_mounted(&self, id: &str, _renderer: &Rc<NativeRenderer>) {
        self.mounted.borrow_mut().push(id.to_string());
    }

    fn cell_ready(&self, id: &str, cell: &ChartCell) {
        self.ready
            .borrow_mut()
            .push((id.to_string(), cell.try_chart().is_some()));
    }

    fn destroy_cell(&self, id: &str) {
        self.destroyed.borrow_mut().push(id.to_string());
        let mut hosts = self.hosts.borrow_mut();
        if let Some(at) = hosts.iter().rposition(|(k, _)| k == id) {
            hosts.remove(at);
        }
    }

    fn present_grid(&self, grid: &GridPresentation) {
        self.grids.borrow_mut().push(grid.clone());
    }

    fn toast(&self, message: &str, kind: ToastKind, _duration_ms: f64) {
        self.toasts.borrow_mut().push((message.to_string(), kind));
    }

    fn grid_metrics(&self) -> Option<(GridBox, f64, String)> {
        self.grid_box
            .get()
            .map(|g| (g, self.dpr.get(), "rgb(40, 40, 40)".to_string()))
    }

    fn cell_box(&self, id: &str) -> Option<CellBox> {
        self.cell_boxes.borrow().get(id).copied()
    }

    fn screenshot_surface(&self, width: f64, height: f64) -> Option<Rc<dyn ScreenshotSurface>> {
        let surface = Rc::new(TestSurface {
            canvas: Rc::new(RecordingCanvas::new()),
            size: (width, height),
            overlays: RefCell::new(Vec::new()),
        });
        self.surfaces.borrow_mut().push(surface.clone());
        Some(surface)
    }
}
