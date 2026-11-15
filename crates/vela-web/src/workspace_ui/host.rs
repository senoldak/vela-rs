//! [`BrowserWorkspaceHost`]: [`WorkspaceHost`] on the DOM, the browser half of
//! `VelaWorkspace.ts` and `ChartCell.ts`.
//!
//! # What is real
//!
//! Every member of the seam:
//!
//! | Member             | What the browser does                                                  |
//! |--------------------|------------------------------------------------------------------------|
//! | `create_cell`      | `div.vela-cell[data-cell-id]` in the grid (relative, clipped), a        |
//! |                    | [`BrowserHost`] on it, capture-phase `pointerdown` and `focusin`        |
//! |                    | listeners that activate the cell                                        |
//! | `cell_mounted`     | the renderer-side binding of a single chart ([`crate::binding`]): the   |
//! |                    | drawings widgets, legend, dialogs, input listeners, size observers      |
//! | `cell_ready`       | the engine-side part, at once (the cell's chart exists): translators,   |
//! |                    | dialog sources, the theme echo, the contributed legend providers        |
//! | `destroy_cell`     | inputs, observers, host and container removed                           |
//! | `present_grid`     | template, `data-multi`, `data-maximized`, each cell's `grid-area`,      |
//! |                    | `z-index`, `visibility`, `data-active`, `data-drop-target`, DOM slot    |
//! |                    | order, the attribution mark into the bottom-left cell, the strips       |
//! | `toast`            | the status pill ([`ToastView`])                                         |
//! | `grid_metrics`     | the grid's viewport box, `devicePixelRatio`, computed seam color        |
//! | `cell_box`         | the container's viewport box, `hidden` while a maximize hides it        |
//! | `screenshot_surface` | an offscreen canvas, encoded with `toDataURL('image/png')`            |
//!
//! # Re-entrancy
//!
//! The workspace calls these members while an operation is under way, so nothing here calls
//! back into it. Work that needs the workspace (the splitter strips) is queued with
//! [`WidgetRoot::defer`]; the chart-side binding needs only the cell's own chart, which
//! `cell_ready` hands over. A DOM event that fires while a member runs (a
//! `focusin` raised by a focus call inside the operation) is queued the same way instead
//! of activating the cell in the middle of the operation.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use vela_core::chart_types::ChartTypeRegistry;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::render::canvas::Canvas2d;
use vela_core::render::native::{NativeHost, NativeRenderer};
use vela_core::runtime::Platform;
use vela_core::workspace::screenshot::{DestRect, OverlayPhase};
use vela_core::workspace::{
    CellBox, ChartCell, GridBox, GridPresentation, GridStyles, ScreenshotSurface, ToastKind,
    Workspace, WorkspaceEvent, WorkspaceEventName, WorkspaceHost,
};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlCanvasElement, HtmlElement};

use crate::binding::{self, ChartInputs};
use crate::canvas::BrowserCanvas;
use crate::dom::{Listener, attr, create_element, make, set_style, set_styles, vela_dpr};
use crate::host::BrowserHost;

use super::js;
use super::root::WidgetRoot;

struct CellEntry {
    el: Element,
    host: Rc<BrowserHost>,
    inputs: Option<ChartInputs>,
    _listeners: Vec<Listener>,
    themed: bool,
}

/// The browser's [`WorkspaceHost`]. See the module documentation.
pub struct BrowserWorkspaceHost {
    me: Weak<BrowserWorkspaceHost>,
    root: Rc<WidgetRoot>,
    platform: Rc<dyn Platform>,
    registry: Rc<RefCell<ChartTypeRegistry>>,
    cells: RefCell<BTreeMap<String, CellEntry>>,
    /// The ids placed in the grid, in DOM order.
    placed: RefCell<Vec<String>>,
    busy: Cell<u32>,
    subs: RefCell<Vec<vela_core::workspace::WorkspaceSubscription>>,
}

struct BusyGuard<'a>(&'a Cell<u32>);

impl<'a> BusyGuard<'a> {
    fn new(cell: &'a Cell<u32>) -> Self {
        cell.set(cell.get() + 1);
        Self(cell)
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// An offscreen canvas the layout screenshot is composited on.
struct BrowserSurface {
    canvas: HtmlCanvasElement,
    ctx: Rc<dyn Canvas2d>,
    /// The same context, for the DOM overlay raster.
    raw: web_sys::CanvasRenderingContext2d,
    host: Weak<BrowserWorkspaceHost>,
}

impl ScreenshotSurface for BrowserSurface {
    fn context(&self) -> Rc<dyn Canvas2d> {
        self.ctx.clone()
    }

    fn encode_png(&self) -> Option<String> {
        js::vela_ws_to_png(self.canvas.as_ref())
    }

    fn paint_overlays(&self, cell_id: &str, phase: OverlayPhase, dest: &DestRect) {
        let Some(host) = self.host.upgrade().and_then(|h| h.cell_host(cell_id)) else {
            return;
        };
        crate::raster::paint_overlays(
            &self.raw,
            host.container(),
            host.plot(),
            phase,
            (dest.dx, dest.dy, dest.dw),
        );
    }
}

impl BrowserWorkspaceHost {
    /// A host over `root`; `registry` is the environment's chart-type registry (the
    /// dialogs' chart-type tabs read it).
    pub fn new(root: Rc<WidgetRoot>, registry: Rc<RefCell<ChartTypeRegistry>>) -> Rc<Self> {
        let platform = root.platform().clone();
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            root,
            platform,
            registry,
            cells: RefCell::default(),
            placed: RefCell::default(),
            busy: Cell::new(0),
            subs: RefCell::default(),
        })
    }

    /// The root this host presents on.
    pub fn root(&self) -> &Rc<WidgetRoot> {
        &self.root
    }

    /// The [`BrowserHost`] of a live cell.
    pub fn cell_host(&self, id: &str) -> Option<Rc<BrowserHost>> {
        self.cells.borrow().get(id).map(|c| c.host.clone())
    }

    /// Connect the finished workspace: the root learns it, the layout mode and the wall
    /// clock reach the cells, and the language of the chrome follows it.
    pub fn attach(&self, ws: &Workspace) {
        self.root.set_workspace(ws);
        let mode = self.root.layout_mode();
        ws.set_layout_mode(match mode {
            vela_core::widget::layout_mode::LayoutMode::Mobile => {
                vela_core::ports::chart_renderer::LayoutMode::Mobile
            }
            vela_core::widget::layout_mode::LayoutMode::Desktop => {
                vela_core::ports::chart_renderer::LayoutMode::Desktop
            }
        });
        ws.set_wall_clock(Some(crate::host::SecondClock::new(self.platform.clone())
            as Rc<dyn vela_core::ports::chart_renderer::WallClock>));
        let me = self.me.clone();
        let sub = ws.on(WorkspaceEventName::LocaleChanged, move |ev| {
            let (Some(host), WorkspaceEvent::LocaleChanged(change)) = (me.upgrade(), ev) else {
                return;
            };
            host.relocalize(&change.locale, change.direction);
        });
        self.subs.borrow_mut().push(sub);
        self.root.relabel_mark();
        self.root.schedule_strips();
    }

    /// Detach the subscriptions (the workspace is going away).
    pub fn detach(&self, ws: &Workspace) {
        for sub in self.subs.borrow_mut().drain(..) {
            ws.off(sub);
        }
    }

    fn relocalize(&self, locale: &str, direction: vela_core::intl::TextDirection) {
        let dir = match direction {
            vela_core::intl::TextDirection::Rtl => "rtl",
            vela_core::intl::TextDirection::Ltr => "ltr",
        };
        // `lang` once a locale was set; `dir` only once the host chose a direction.
        self.root
            .set_locale_attributes(Some(locale), self.root.dir_explicit().then_some(dir));
        self.root.relabel_mark();
        let Some(ws) = self.root.workspace() else {
            return;
        };
        for cell in ws.cells() {
            let (Some(host), Some(chart)) = (self.cell_host(cell.id()), cell.try_chart()) else {
                continue;
            };
            binding::rebind_translators(&host, &chart);
        }
    }

    /// The engine-side binding of one cell, from its chart: the translators and dialog
    /// sources, the theme echo and the contributed legend providers.
    fn bind_cell(&self, id: &str, cell: &ChartCell) {
        let Some(host) = self.cell_host(id) else {
            return;
        };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        binding::bind_chart(&host, &chart);
        self.root.bind_legends(&chart);
        let newly = {
            let mut cells = self.cells.borrow_mut();
            match cells.get_mut(id) {
                Some(entry) => !std::mem::replace(&mut entry.themed, true),
                None => false,
            }
        };
        if newly {
            // A theme picked in one cell's settings re-skins the workspace (core does
            // that); the chrome's tokens follow through this echo.
            let root = Rc::downgrade(&self.root);
            let _ = chart.on(ChartEventName::ThemeChanged, move |ev| {
                if let (Some(root), ChartEvent::ThemeChanged(theme)) = (root.upgrade(), ev) {
                    root.set_theme(theme.clone());
                }
            });
        }
    }

    fn activate(&self, id: &str) {
        if self.busy.get() > 0 {
            let me = self.me.clone();
            let id = id.to_string();
            self.root.defer(move || {
                if let Some(host) = me.upgrade() {
                    host.activate(&id);
                }
            });
            return;
        }
        if let Some(ws) = self.root.workspace() {
            ws.activate(id);
        }
    }

    fn drop_entry(&self, id: &str) {
        let entry = self.cells.borrow_mut().remove(id);
        if let Some(mut entry) = entry {
            entry.inputs.take();
            entry.host.dispose();
            entry.el.remove();
        }
        self.root.forget_cell(id);
        self.placed.borrow_mut().retain(|p| p != id);
    }

    /// The cell the grid's attribution mark floats in: the maximized cell while one covers
    /// the grid, else the bottom-left slot's cell.
    fn bottom_left(grid: &GridPresentation) -> Option<&str> {
        if let Some(max) = &grid.maximized {
            return Some(max.as_str());
        }
        let by_area = grid.areas.as_ref().and_then(|rows| {
            let first = rows.last()?.split_whitespace().next()?.to_string();
            grid.cells
                .iter()
                .find(|c| c.grid_area.as_deref() == Some(first.as_str()))
        });
        let cell = by_area.or_else(|| {
            let slot = grid.rows.len().saturating_sub(1) * grid.cols.len();
            grid.cells.iter().find(|c| c.slot == slot)
        });
        cell.or_else(|| grid.cells.first()).map(|c| c.id.as_str())
    }
}

impl WorkspaceHost for BrowserWorkspaceHost {
    fn create_cell(&self, id: &str) -> Rc<dyn NativeHost> {
        let _busy = BusyGuard::new(&self.busy);
        self.drop_entry(id); // a rebuilt cell starts from a fresh container
        let doc = self.root.document().clone();
        let el = make(&doc, "div", "vela-cell");
        attr(&el, "data-cell-id", id);
        set_styles(&el, &[("position", "relative"), ("overflow", "hidden")]);
        let _ = self.root.grid().append_child(&el);
        let host = BrowserHost::new(&el, self.platform.clone())
            .expect("a container in the document builds a chart host");
        let mut listeners = Vec::new();
        // The press reaches the cell before the chart sees it (capture), focus anywhere
        // inside it (a dialog field, the data canvas) activates it too.
        for (name, capture) in [("pointerdown", true), ("focusin", false)] {
            let (me, cell) = (self.me.clone(), id.to_string());
            listeners.push(Listener::new(el.as_ref(), name, capture, true, move |_| {
                if let Some(host) = me.upgrade() {
                    host.activate(&cell);
                }
            }));
        }
        self.root.register_cell(id, &el);
        self.cells.borrow_mut().insert(
            id.to_string(),
            CellEntry {
                el,
                host: host.clone(),
                inputs: None,
                _listeners: listeners,
                themed: false,
            },
        );
        host
    }

    fn cell_mounted(&self, id: &str, renderer: &Rc<NativeRenderer>) {
        let _busy = BusyGuard::new(&self.busy);
        let Some(host) = self.cell_host(id) else {
            return;
        };
        binding::bind_renderer(&host, renderer, self.registry.clone());
        // The chart dialogs open over the whole widget, as the original's `dialogHost: root`.
        host.dialogs
            .set_dialog_host(Some(self.root.element().clone()));
        let inputs = ChartInputs::attach(renderer.clone(), host, self.platform.clone());
        if let Some(entry) = self.cells.borrow_mut().get_mut(id) {
            entry.inputs = Some(inputs);
        }
    }

    fn cell_ready(&self, id: &str, cell: &ChartCell) {
        let _busy = BusyGuard::new(&self.busy);
        self.bind_cell(id, cell);
    }

    fn destroy_cell(&self, id: &str) {
        let _busy = BusyGuard::new(&self.busy);
        self.drop_entry(id);
    }

    fn present_grid(&self, grid: &GridPresentation) {
        let _busy = BusyGuard::new(&self.busy);
        let el = self.root.grid();
        if grid.multi {
            attr(el, "data-multi", "1");
        } else {
            let _ = el.remove_attribute("data-multi");
        }
        if grid.maximized.is_some() {
            attr(el, "data-maximized", "1");
        } else {
            let _ = el.remove_attribute("data-maximized");
        }
        let styles = GridStyles {
            cols: grid.cols.clone(),
            rows: grid.rows.clone(),
            areas: grid.areas.clone(),
            per_cell: BTreeMap::new(),
        };
        set_styles(
            el,
            &[
                ("grid-template-columns", &styles.template_columns()),
                ("grid-template-rows", &styles.template_rows()),
                (
                    "grid-template-areas",
                    &styles.template_areas().unwrap_or_default(),
                ),
            ],
        );
        {
            let live: Vec<Option<Element>> = {
                let cells = self.cells.borrow();
                grid.cells
                    .iter()
                    .map(|p| cells.get(&p.id).map(|e| e.el.clone()))
                    .collect()
            };
            for (p, cell) in grid.cells.iter().zip(&live) {
                let Some(cell) = cell else {
                    continue;
                };
                set_style(cell, "grid-area", p.grid_area.as_deref().unwrap_or(""));
                set_style(
                    cell,
                    "z-index",
                    &p.z_index.map(|z| z.to_string()).unwrap_or_default(),
                );
                set_style(cell, "visibility", if p.hidden { "hidden" } else { "" });
                if p.active {
                    attr(cell, "data-active", "1");
                } else {
                    let _ = cell.remove_attribute("data-active");
                }
                if p.drop_target {
                    attr(cell, "data-drop-target", "1");
                } else {
                    let _ = cell.remove_attribute("data-drop-target");
                }
            }
        }
        // Slot order in the DOM (an auto-flow grid places by it). Only when it differs: a
        // move would drop the focus and a pointer capture inside the cell, and a splitter
        // drag presents the grid on every move.
        let wanted: Vec<String> = grid.cells.iter().map(|c| c.id.clone()).collect();
        let reorder = *self.placed.borrow() != wanted;
        if reorder {
            // Moving a node out of the DOM can make the browser blur what had focus inside
            // it, and a blur handler may reach back into this host: no borrow is held while
            // the nodes move.
            let moving: Vec<Element> = {
                let cells = self.cells.borrow();
                wanted
                    .iter()
                    .filter_map(|id| cells.get(id).map(|e| e.el.clone()))
                    .collect()
            };
            *self.placed.borrow_mut() = wanted;
            for el in &moving {
                let _ = self.root.grid().append_child(el);
            }
        }
        let host_el = Self::bottom_left(grid)
            .and_then(|id| self.cells.borrow().get(id).map(|c| c.el.clone()));
        self.root.place_mark(host_el.as_ref());
        self.root.schedule_strips();
    }

    fn toast(&self, message: &str, kind: ToastKind, duration_ms: f64) {
        self.root.toast(message, kind, duration_ms);
    }

    fn grid_metrics(&self) -> Option<(GridBox, f64, String)> {
        let rect = self.root.grid().get_bounding_client_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        Some((
            GridBox {
                left: rect.left(),
                top: rect.top(),
                width: rect.width(),
                height: rect.height(),
            },
            vela_dpr(),
            self.root.seam_color(),
        ))
    }

    fn cell_box(&self, id: &str) -> Option<CellBox> {
        let cells = self.cells.borrow();
        let el = &cells.get(id)?.el;
        let rect = el.get_bounding_client_rect();
        let hidden = el
            .unchecked_ref::<HtmlElement>()
            .style()
            .get_property_value("visibility")
            .is_ok_and(|v| v == "hidden");
        Some(CellBox {
            hidden,
            left: rect.left(),
            top: rect.top(),
            width: rect.width(),
            height: rect.height(),
        })
    }

    fn screenshot_surface(&self, width: f64, height: f64) -> Option<Rc<dyn ScreenshotSurface>> {
        let canvas: HtmlCanvasElement =
            create_element(self.root.document(), "canvas").unchecked_into();
        canvas.set_width(width.max(1.0) as u32);
        canvas.set_height(height.max(1.0) as u32);
        let ctx = canvas.get_context("2d").ok().flatten()?;
        let ctx: web_sys::CanvasRenderingContext2d = ctx.unchecked_into();
        Some(Rc::new(BrowserSurface {
            canvas,
            ctx: Rc::new(BrowserCanvas::new(ctx.clone())),
            raw: ctx,
            host: self.me.clone(),
        }))
    }
}

impl BrowserWorkspaceHost {
    /// Remove every cell (the workspace is destroyed; it already asked for each one).
    pub fn dispose(&self) {
        let ids: Vec<String> = self.cells.borrow().keys().cloned().collect();
        for id in ids {
            self.drop_entry(&id);
        }
    }
}
