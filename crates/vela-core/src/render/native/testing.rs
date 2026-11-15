//! An in-memory [`NativeHost`] for tests, and the reference implementation of the
//! seam for the browser crate's author.
//!
//! Every canvas is a [`RecordingCanvas`]; frames and the two clocks are stepped by
//! hand ([`TestHost::step_frame`], [`TestHost::run_until_idle`], [`TestHost::advance`]);
//! every presentation effect the shell pushes is recorded and readable.
//!
//! ```ignore
//! let host = TestHost::new(800.0, 400.0, 1.0);
//! let renderer = NativeRenderer::new(host.clone(), None);
//! renderer.mount(&dark_theme());
//! renderer.set_bars(&bars, SetBarsOptions::default());
//! host.run_until_idle();
//! assert!(!host.canvas(&CanvasSlot::Data).unwrap().is_empty());
//! ```

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use serde_json::Value;

use crate::model::Inputs;
use crate::ports::chart_renderer::{
    IndicatorStatus, LayoutMode, RendererRootAttributes, VelaTheme, WallClock,
};
use crate::render::canvas::{Canvas2d, CanvasImage, RecordingCanvas};
use crate::render::core::scheduler::FrameTask;
use crate::render::drawings::indicator_slices::SliceSurface;
use crate::render::drawings::user_controller::DrawingsUiEvent;
use crate::render::gl::{Gl, RecordingGl};
use crate::runtime::Unsubscribe;

use super::host::{
    AttributionState, CanvasSlot, HostCanvas, HostGlCanvas, HostMetrics, LayoutFacts,
    LegendPlotValue, LegendRow, MarkPopoverAction, NativeHost, ScrollButtonState,
};

/// One canvas of the test pile.
pub struct TestCanvas {
    ctx: Rc<RecordingCanvas>,
    backing: Cell<(f64, f64)>,
    css: Cell<(f64, f64)>,
    resizes: Cell<usize>,
    image: CanvasImage,
}

impl TestCanvas {
    fn new(label: &str) -> Rc<Self> {
        Rc::new(Self {
            ctx: Rc::new(RecordingCanvas::new()),
            backing: Cell::new((300.0, 150.0)),
            css: Cell::new((300.0, 150.0)),
            resizes: Cell::new(0),
            image: Rc::new(label.to_string()),
        })
    }

    /// The recording context everything painted on this canvas.
    pub fn recording(&self) -> Rc<RecordingCanvas> {
        self.ctx.clone()
    }

    /// Backing size in device px.
    pub fn backing_size(&self) -> (f64, f64) {
        self.backing.get()
    }

    /// Display size in CSS px.
    pub fn css_size(&self) -> (f64, f64) {
        self.css.get()
    }

    /// How many times the canvas was resized (each resize clears it).
    pub fn resize_count(&self) -> usize {
        self.resizes.get()
    }
}

impl HostCanvas for TestCanvas {
    fn context(&self) -> Rc<dyn Canvas2d> {
        self.ctx.clone()
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }

    fn set_size(&self, bw: f64, bh: f64, cw: f64, ch: f64) {
        self.backing.set((bw, bh));
        self.css.set((cw, ch));
        self.resizes.set(self.resizes.get() + 1);
        self.ctx.clear(); // assigning canvas.width clears the bitmap
    }
}

/// The data canvas of a host that provides WebGL2 ([`TestHost::provide_gl`]).
pub struct TestGlCanvas {
    gl: Rc<RecordingGl>,
    backing: Cell<(f64, f64)>,
    css: Cell<(f64, f64)>,
    resizes: Cell<usize>,
    image: CanvasImage,
}

impl TestGlCanvas {
    /// The recording context everything was drawn with.
    pub fn recording(&self) -> Rc<RecordingGl> {
        self.gl.clone()
    }

    /// Backing size in device px.
    pub fn backing_size(&self) -> (f64, f64) {
        self.backing.get()
    }

    /// Display size in CSS px.
    pub fn css_size(&self) -> (f64, f64) {
        self.css.get()
    }

    pub fn resize_count(&self) -> usize {
        self.resizes.get()
    }
}

impl HostGlCanvas for TestGlCanvas {
    fn gl(&self) -> Rc<dyn Gl> {
        self.gl.clone()
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }

    fn set_size(&self, bw: f64, bh: f64, cw: f64, ch: f64) {
        self.backing.set((bw, bh));
        self.css.set((cw, ch));
        self.resizes.set(self.resizes.get() + 1);
    }
}

/// One offscreen slice canvas.
pub struct TestSlice {
    ctx: Rc<RecordingCanvas>,
    size: Cell<(u32, u32)>,
    image: CanvasImage,
}

impl TestSlice {
    pub fn recording(&self) -> Rc<RecordingCanvas> {
        self.ctx.clone()
    }
}

impl SliceSurface for TestSlice {
    fn size(&self) -> (u32, u32) {
        self.size.get()
    }

    fn resize(&self, width: u32, height: u32) {
        self.size.set((width, height));
        self.ctx.clear();
    }

    fn context(&self) -> Option<Rc<dyn Canvas2d>> {
        Some(self.ctx.clone())
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }
}

type ClockSubs = Rc<RefCell<Vec<(u64, Rc<RefCell<Box<dyn FnMut(f64)>>>)>>>;

/// A wall clock ticked by hand.
#[derive(Default)]
pub struct TestWallClock {
    next: Cell<u64>,
    subs: ClockSubs,
}

impl TestWallClock {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Run every subscriber with `now`.
    pub fn tick(&self, now: f64) {
        let subs: Vec<_> = self.subs.borrow().iter().map(|(_, s)| s.clone()).collect();
        for s in subs {
            if let Ok(mut f) = s.try_borrow_mut() {
                (**f)(now);
            }
        }
    }

    pub fn subscribers(&self) -> usize {
        self.subs.borrow().len()
    }
}

impl WallClock for TestWallClock {
    fn on_tick(&self, cb: Box<dyn FnMut(f64)>) -> Unsubscribe {
        let id = self.next.get();
        self.next.set(id + 1);
        self.subs.borrow_mut().push((id, Rc::new(RefCell::new(cb))));
        let subs = self.subs.clone();
        Box::new(move || subs.borrow_mut().retain(|(i, _)| *i != id))
    }
}

/// The in-memory host. See the module documentation.
pub struct TestHost {
    metrics: Cell<HostMetrics>,
    toolbar_gutter: Cell<f64>,
    canvases: RefCell<HashMap<CanvasSlot, Rc<TestCanvas>>>,
    gl_offered: Cell<bool>,
    gl: Rc<RecordingGl>,
    gl_canvas: RefCell<Option<Rc<TestGlCanvas>>>,
    removed: RefCell<Vec<CanvasSlot>>,
    layer_order: RefCell<Option<(Vec<String>, Vec<String>)>>,
    layer_order_calls: Cell<usize>,
    slices: RefCell<Vec<Rc<TestSlice>>>,
    frames: RefCell<BTreeMap<u32, FrameTask>>,
    next_frame: Cell<u32>,
    now: Cell<f64>,
    wall: Cell<f64>,
    clock: RefCell<Option<Rc<TestWallClock>>>,
    // recorded effects
    background: RefCell<Option<String>>,
    chrome_themes: RefCell<Vec<VelaTheme>>,
    root_attrs: RefCell<Vec<RendererRootAttributes>>,
    layouts: RefCell<Vec<LayoutFacts>>,
    loading: Cell<bool>,
    loading_log: RefCell<Vec<bool>>,
    scroll_button: RefCell<Vec<ScrollButtonState>>,
    attributions: RefCell<Vec<AttributionState>>,
    popovers: RefCell<Vec<MarkPopoverAction>>,
    announcements: RefCell<Vec<String>>,
    shade: RefCell<Option<(f64, f64, f64)>>,
    keyboard: Cell<Option<bool>>,
    features: RefCell<Vec<(String, Value)>>,
    layout_modes: RefCell<Vec<LayoutMode>>,
    warnings: RefCell<Vec<String>>,
    legend_rows: RefCell<Vec<LegendRow>>,
    legend_removed: RefCell<Vec<String>>,
    legend_statuses: RefCell<Vec<(String, IndicatorStatus)>>,
    legend_panes: RefCell<Vec<(String, String)>>,
    legend_visible: RefCell<Vec<(String, bool)>>,
    legend_order: RefCell<Vec<String>>,
    legend_collapsed: RefCell<Vec<(String, Option<String>)>>,
    legend_values: RefCell<Vec<(String, Vec<LegendPlotValue>)>>,
    layout_changes: Cell<usize>,
    mark_icons_used: Cell<bool>,
    drawings_ui: RefCell<Vec<DrawingsUiEvent>>,
}

impl TestHost {
    /// A host whose wrapper is `width` x `height` CSS px at `dpr`.
    pub fn new(width: f64, height: f64, dpr: f64) -> Rc<Self> {
        Rc::new(Self {
            metrics: Cell::new(HostMetrics {
                client_width: width,
                client_height: height,
                plot_width: width,
                plot_height: height,
                dpr,
                device_size: None,
            }),
            toolbar_gutter: Cell::new(0.0),
            canvases: RefCell::new(HashMap::new()),
            gl_offered: Cell::new(false),
            gl: Rc::new(RecordingGl::new()),
            gl_canvas: RefCell::new(None),
            removed: RefCell::new(Vec::new()),
            layer_order: RefCell::new(None),
            layer_order_calls: Cell::new(0),
            slices: RefCell::new(Vec::new()),
            frames: RefCell::new(BTreeMap::new()),
            next_frame: Cell::new(1),
            now: Cell::new(1_000.0),
            wall: Cell::new(1_700_000_000_000.0),
            clock: RefCell::new(None),
            background: RefCell::new(None),
            chrome_themes: RefCell::new(Vec::new()),
            root_attrs: RefCell::new(Vec::new()),
            layouts: RefCell::new(Vec::new()),
            loading: Cell::new(false),
            loading_log: RefCell::new(Vec::new()),
            scroll_button: RefCell::new(Vec::new()),
            attributions: RefCell::new(Vec::new()),
            popovers: RefCell::new(Vec::new()),
            announcements: RefCell::new(Vec::new()),
            shade: RefCell::new(None),
            keyboard: Cell::new(None),
            features: RefCell::new(Vec::new()),
            layout_modes: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            legend_rows: RefCell::new(Vec::new()),
            legend_removed: RefCell::new(Vec::new()),
            legend_statuses: RefCell::new(Vec::new()),
            legend_panes: RefCell::new(Vec::new()),
            legend_visible: RefCell::new(Vec::new()),
            legend_order: RefCell::new(Vec::new()),
            legend_collapsed: RefCell::new(Vec::new()),
            legend_values: RefCell::new(Vec::new()),
            layout_changes: Cell::new(0),
            mark_icons_used: Cell::new(false),
            drawings_ui: RefCell::new(Vec::new()),
        })
    }

    // ── geometry ──

    /// Resize the wrapper (the browser's resize observer would then call `resize`).
    pub fn set_size(&self, width: f64, height: f64) {
        let mut m = self.metrics.get();
        m.client_width = width;
        m.client_height = height;
        m.plot_width = width - self.toolbar_gutter.get();
        m.plot_height = height;
        self.metrics.set(m);
    }

    pub fn set_dpr(&self, dpr: f64) {
        let mut m = self.metrics.get();
        m.dpr = dpr;
        self.metrics.set(m);
    }

    /// Report an exact device-pixel box (`devicePixelContentBoxSize`).
    pub fn set_device_size(&self, size: Option<(f64, f64)>) {
        let mut m = self.metrics.get();
        m.device_size = size;
        self.metrics.set(m);
    }

    // ── canvases ──

    pub fn test_canvas(&self, slot: &CanvasSlot) -> Option<Rc<TestCanvas>> {
        self.canvases.borrow().get(slot).cloned()
    }

    /// Opt in: offer a WebGL2 data canvas (a [`RecordingGl`] shared by every GL canvas
    /// this host hands out) through [`NativeHost::create_gl_canvas`]. Off by default, so
    /// the data canvas is the usual recording 2D one. Call before `mount`.
    pub fn provide_gl(&self, offered: bool) {
        self.gl_offered.set(offered);
    }

    /// The recording GL context behind the offered data canvas: configure its failure
    /// knobs before `mount`, read its stream after.
    pub fn gl(&self) -> Rc<RecordingGl> {
        self.gl.clone()
    }

    /// The GL data canvas currently on the plot, if the data canvas is a GL one.
    pub fn gl_canvas(&self) -> Option<Rc<TestGlCanvas>> {
        self.gl_canvas.borrow().clone()
    }

    /// The recording of one canvas.
    pub fn canvas(&self, slot: &CanvasSlot) -> Option<Rc<RecordingCanvas>> {
        self.test_canvas(slot).map(|c| c.recording())
    }

    /// Every slot currently on the plot.
    pub fn slots(&self) -> Vec<CanvasSlot> {
        let mut s: Vec<CanvasSlot> = self.canvases.borrow().keys().cloned().collect();
        s.sort_by_key(|k| format!("{k:?}"));
        s
    }

    /// Slots removed since the host was created, in order.
    pub fn removed_slots(&self) -> Vec<CanvasSlot> {
        self.removed.borrow().clone()
    }

    /// The last `order_layer_canvases` call: `(below, above)` channels.
    pub fn layer_order(&self) -> Option<(Vec<String>, Vec<String>)> {
        self.layer_order.borrow().clone()
    }

    pub fn layer_order_calls(&self) -> usize {
        self.layer_order_calls.get()
    }

    /// The slice canvases created so far.
    pub fn slice_surfaces(&self) -> Vec<Rc<TestSlice>> {
        self.slices.borrow().clone()
    }

    /// Clear every canvas recording.
    pub fn clear_canvases(&self) {
        for c in self.canvases.borrow().values() {
            c.ctx.clear();
        }
    }

    // ── frames and clocks ──

    /// Frames armed and not yet run.
    pub fn pending_frames(&self) -> usize {
        self.frames.borrow().len()
    }

    /// Advance the frame clock by `dt_ms` and run the frames armed at entry (frames
    /// armed by them wait for the next step). Returns how many ran.
    pub fn step_frame(&self, dt_ms: f64) -> usize {
        self.now.set(self.now.get() + dt_ms);
        self.wall.set(self.wall.get() + dt_ms);
        let due: Vec<u32> = self.frames.borrow().keys().copied().collect();
        let mut ran = 0;
        for id in due {
            let task = self.frames.borrow_mut().remove(&id);
            if let Some(task) = task {
                task();
                ran += 1;
            }
        }
        ran
    }

    /// Step 16 ms frames until none is armed; returns the number of steps (panics
    /// past `max_steps` frames: an animation that never settles).
    pub fn run_until_idle(&self) -> usize {
        self.run_until_idle_max(2_000)
    }

    pub fn run_until_idle_max(&self, max_steps: usize) -> usize {
        let mut steps = 0;
        while self.pending_frames() > 0 {
            assert!(
                steps < max_steps,
                "frames never settled after {max_steps} steps"
            );
            self.step_frame(16.0);
            steps += 1;
        }
        steps
    }

    /// Advance both clocks without running frames.
    pub fn advance(&self, ms: f64) {
        self.now.set(self.now.get() + ms);
        self.wall.set(self.wall.get() + ms);
    }

    pub fn set_now(&self, ms: f64) {
        self.now.set(ms);
    }

    pub fn set_wall(&self, ms: f64) {
        self.wall.set(ms);
    }

    /// Provide the renderer's own second pulse (the countdown chip's source).
    pub fn with_clock(&self) -> Rc<TestWallClock> {
        let clock = TestWallClock::new();
        *self.clock.borrow_mut() = Some(clock.clone());
        clock
    }

    // ── recorded effects ──

    pub fn background(&self) -> Option<String> {
        self.background.borrow().clone()
    }

    pub fn chrome_themes(&self) -> Vec<VelaTheme> {
        self.chrome_themes.borrow().clone()
    }

    pub fn root_attributes(&self) -> Vec<RendererRootAttributes> {
        self.root_attrs.borrow().clone()
    }

    pub fn layouts(&self) -> Vec<LayoutFacts> {
        self.layouts.borrow().clone()
    }

    pub fn last_layout(&self) -> Option<LayoutFacts> {
        self.layouts.borrow().last().copied()
    }

    pub fn loading_visible(&self) -> bool {
        self.loading.get()
    }

    pub fn loading_log(&self) -> Vec<bool> {
        self.loading_log.borrow().clone()
    }

    pub fn scroll_button_states(&self) -> Vec<ScrollButtonState> {
        self.scroll_button.borrow().clone()
    }

    pub fn last_scroll_button(&self) -> Option<ScrollButtonState> {
        self.scroll_button.borrow().last().copied()
    }

    pub fn attributions(&self) -> Vec<AttributionState> {
        self.attributions.borrow().clone()
    }

    pub fn mark_popovers(&self) -> Vec<MarkPopoverAction> {
        self.popovers.borrow().clone()
    }

    pub fn announcements(&self) -> Vec<String> {
        self.announcements.borrow().clone()
    }

    pub fn shade_right_area(&self) -> Option<(f64, f64, f64)> {
        *self.shade.borrow()
    }

    pub fn keyboard_enabled(&self) -> Option<bool> {
        self.keyboard.get()
    }

    pub fn feature_changes(&self) -> Vec<(String, Value)> {
        self.features.borrow().clone()
    }

    pub fn layout_modes(&self) -> Vec<LayoutMode> {
        self.layout_modes.borrow().clone()
    }

    pub fn warnings(&self) -> Vec<String> {
        self.warnings.borrow().clone()
    }

    pub fn legend_rows(&self) -> Vec<LegendRow> {
        self.legend_rows.borrow().clone()
    }

    pub fn legend_removed(&self) -> Vec<String> {
        self.legend_removed.borrow().clone()
    }

    pub fn legend_statuses(&self) -> Vec<(String, IndicatorStatus)> {
        self.legend_statuses.borrow().clone()
    }

    pub fn legend_panes(&self) -> Vec<(String, String)> {
        self.legend_panes.borrow().clone()
    }

    pub fn legend_visibility(&self) -> Vec<(String, bool)> {
        self.legend_visible.borrow().clone()
    }

    pub fn legend_order(&self) -> Vec<String> {
        self.legend_order.borrow().clone()
    }

    pub fn legend_collapsed(&self) -> Vec<(String, Option<String>)> {
        self.legend_collapsed.borrow().clone()
    }

    /// The last plot values pushed to the legend.
    pub fn legend_plot_values(&self) -> Vec<(String, Vec<LegendPlotValue>)> {
        self.legend_values.borrow().clone()
    }

    /// How many `layout_changed` notifications were received.
    pub fn layout_change_count(&self) -> usize {
        self.layout_changes.get()
    }

    /// Every user-drawings UI event the controller reported, in order.
    pub fn drawings_ui_events(&self) -> Vec<DrawingsUiEvent> {
        self.drawings_ui.borrow().clone()
    }

    /// Forget the recorded user-drawings UI events.
    pub fn clear_drawings_ui_events(&self) {
        self.drawings_ui.borrow_mut().clear();
    }

    pub fn mark_icons_requested(&self) -> bool {
        self.mark_icons_used.get()
    }
}

impl NativeHost for TestHost {
    fn metrics(&self) -> HostMetrics {
        self.metrics.get()
    }

    fn create_canvas(&self, slot: &CanvasSlot) -> Rc<dyn HostCanvas> {
        let canvas = TestCanvas::new(&format!("{slot:?}"));
        self.canvases
            .borrow_mut()
            .insert(slot.clone(), canvas.clone());
        canvas
    }

    fn create_gl_canvas(&self) -> Option<Rc<dyn HostGlCanvas>> {
        if !self.gl_offered.get() {
            return None;
        }
        let canvas = Rc::new(TestGlCanvas {
            gl: self.gl.clone(),
            backing: Cell::new((300.0, 150.0)),
            css: Cell::new((300.0, 150.0)),
            resizes: Cell::new(0),
            image: Rc::new("Data(gl)".to_string()),
        });
        *self.gl_canvas.borrow_mut() = Some(canvas.clone());
        Some(canvas)
    }

    fn remove_canvas(&self, slot: &CanvasSlot) {
        if *slot == CanvasSlot::Data {
            *self.gl_canvas.borrow_mut() = None;
        }
        self.canvases.borrow_mut().remove(slot);
        self.removed.borrow_mut().push(slot.clone());
    }

    fn order_layer_canvases(&self, below: &[String], above: &[String]) {
        *self.layer_order.borrow_mut() = Some((below.to_vec(), above.to_vec()));
        self.layer_order_calls.set(self.layer_order_calls.get() + 1);
    }

    fn create_slice_surface(&self) -> Rc<dyn SliceSurface> {
        let slice = Rc::new(TestSlice {
            ctx: Rc::new(RecordingCanvas::new()),
            size: Cell::new((0, 0)),
            image: Rc::new(format!("slice{}", self.slices.borrow().len())),
        });
        self.slices.borrow_mut().push(slice.clone());
        slice
    }

    fn request_frame(&self, task: FrameTask) -> u32 {
        let id = self.next_frame.get();
        self.next_frame.set(id + 1);
        self.frames.borrow_mut().insert(id, task);
        id
    }

    fn cancel_frame(&self, handle: u32) {
        self.frames.borrow_mut().remove(&handle);
    }

    fn frame_now_ms(&self) -> f64 {
        self.now.get()
    }

    fn wall_ms(&self) -> f64 {
        self.wall.get()
    }

    fn second_clock(&self) -> Option<Rc<dyn WallClock>> {
        self.clock.borrow().clone().map(|c| c as Rc<dyn WallClock>)
    }

    fn set_background(&self, css_color: &str) {
        *self.background.borrow_mut() = Some(css_color.to_string());
    }

    fn apply_chrome_theme(&self, theme: &VelaTheme) {
        self.chrome_themes.borrow_mut().push(theme.clone());
    }

    fn apply_root_attributes(&self, attrs: &RendererRootAttributes) {
        self.root_attrs.borrow_mut().push(attrs.clone());
    }

    fn publish_layout(&self, layout: &LayoutFacts) {
        // The plot is inset by the toolbar gutter from the left edge.
        self.toolbar_gutter.set(layout.toolbar_gutter);
        let mut m = self.metrics.get();
        m.plot_width = m.client_width - layout.toolbar_gutter;
        self.metrics.set(m);
        self.layouts.borrow_mut().push(*layout);
    }

    fn set_loading_visible(&self, visible: bool) {
        self.loading.set(visible);
        self.loading_log.borrow_mut().push(visible);
    }

    fn scroll_button(&self, state: &ScrollButtonState) {
        self.scroll_button.borrow_mut().push(*state);
    }

    fn attribution(&self, state: &AttributionState) {
        self.attributions.borrow_mut().push(state.clone());
    }

    fn mark_popover(&self, action: &MarkPopoverAction) {
        self.popovers.borrow_mut().push(action.clone());
    }

    fn announce(&self, text: &str) {
        self.announcements.borrow_mut().push(text.to_string());
    }

    fn shade_right(&self, area: Option<(f64, f64, f64)>) {
        *self.shade.borrow_mut() = area;
    }

    fn set_keyboard_enabled(&self, enabled: bool) {
        self.keyboard.set(Some(enabled));
    }

    fn warn(&self, message: &str) {
        self.warnings.borrow_mut().push(message.to_string());
    }

    fn feature_changed(&self, key: &str, value: &Value) {
        self.features
            .borrow_mut()
            .push((key.to_string(), value.clone()));
    }

    fn set_layout_mode(&self, mode: LayoutMode) {
        self.layout_modes.borrow_mut().push(mode);
    }

    fn legend_upsert(&self, row: &LegendRow) {
        self.legend_rows.borrow_mut().push(row.clone());
    }

    fn legend_remove(&self, id: &str) {
        self.legend_removed.borrow_mut().push(id.to_string());
    }

    fn legend_set_values(&self, _id: &str, _values: &Inputs, _props: Option<&Inputs>) {}

    fn legend_set_status(&self, id: &str, status: IndicatorStatus) {
        self.legend_statuses
            .borrow_mut()
            .push((id.to_string(), status));
    }

    fn legend_set_pane(&self, id: &str, pane_id: &str) {
        self.legend_panes
            .borrow_mut()
            .push((id.to_string(), pane_id.to_string()));
    }

    fn legend_set_visible(&self, id: &str, visible: bool) {
        self.legend_visible
            .borrow_mut()
            .push((id.to_string(), visible));
    }

    fn legend_order(&self, ids: &[String]) {
        *self.legend_order.borrow_mut() = ids.to_vec();
    }

    fn legend_collapsed_panes(&self, masters: &[(String, Option<String>)]) {
        *self.legend_collapsed.borrow_mut() = masters.to_vec();
    }

    fn legend_plot_values(&self, values: &[(String, Vec<LegendPlotValue>)]) {
        *self.legend_values.borrow_mut() = values.to_vec();
    }

    fn layout_changed(&self) {
        self.layout_changes.set(self.layout_changes.get() + 1);
    }

    fn drawings_ui(&self, event: &DrawingsUiEvent) {
        self.drawings_ui.borrow_mut().push(event.clone());
    }
}

// ── fixtures ──

use crate::model::{
    IndicatorModel, LineLikeKind, LineLikeStyle, LineStyle, Ohlcv, PaneHint, SeriesData,
    SeriesPoint, SeriesSpec,
};

/// `n` bars `step_ms` apart from `start_ms`, on a gentle wave around 100, each with
/// a volume.
pub fn sample_bars(n: usize, start_ms: i64, step_ms: i64) -> Vec<Ohlcv> {
    (0..n)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.35).sin() * 8.0 + i as f64 * 0.05;
            let open = base;
            let close = base + (i as f64 * 0.9).cos() * 1.5;
            let high = open.max(close) + 1.0;
            let low = open.min(close) - 1.0;
            Ohlcv::new(
                start_ms + i as i64 * step_ms,
                open,
                high,
                low,
                close,
                Some(1_000.0 + (i % 17) as f64 * 50.0),
            )
        })
        .collect()
}

/// A line-series indicator model: `values[i]` is the point of bar `i`
/// (`times[i]`), on `pane_id` (an `overlay` one draws on the price pane).
pub fn line_model(
    id: &str,
    pane_id: &str,
    overlay: bool,
    times: &[i64],
    values: &[Option<f64>],
) -> IndicatorModel {
    IndicatorModel {
        id: id.into(),
        title: id.into(),
        shorttitle: None,
        display_title: None,
        display_shorttitle: None,
        overlay,
        pane_hint: if overlay {
            PaneHint::Price
        } else {
            PaneHint::New
        },
        native: None,
        legend: None,
        pane_axis: None,
        pane_id: Some(pane_id.into()),
        own_scale: false,
        anchor_time: None,
        series: vec![SeriesSpec {
            id: format!("{id}:line"),
            title: id.into(),
            display_title: None,
            pane_id: pane_id.into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::LineLike {
                kind: LineLikeKind::Line,
                points: values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| SeriesPoint {
                        time: times.get(i).copied().unwrap_or(0),
                        value: *v,
                        color: None,
                    })
                    .collect(),
                style: LineLikeStyle {
                    color: "#2962ff".into(),
                    width: 2.0,
                    line_style: LineStyle::Solid,
                    base: None,
                },
            },
        }],
        fills: vec![],
        backgrounds: vec![],
        price_lines: vec![],
        lines: vec![],
        boxes: vec![],
        labels: vec![],
        polylines: vec![],
        linefills: vec![],
        tables: vec![],
        bar_colors: vec![],
        trades: vec![],
        inputs: vec![],
        input_values: Default::default(),
        props: vec![],
        prop_values: Default::default(),
    }
}
