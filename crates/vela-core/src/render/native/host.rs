//! The host seam of the native renderer shell: everything the original
//! `NativeRenderer` reads from or does to the DOM, reduced to one small trait.
//!
//! `vela-core` has no DOM, so the shell ([`NativeRenderer`](super::NativeRenderer))
//! is written against [`NativeHost`] and the browser crate implements it. The
//! reference implementation is [`TestHost`](super::testing::TestHost).
//!
//! The members fall in three groups:
//!
//! 1. **Required**: the plot geometry (pulled by [`NativeHost::metrics`], as
//!    `syncSize` reads `clientWidth` / `getBoundingClientRect` /
//!    `devicePixelRatio` in the original), the stacked canvases of the pile, the
//!    frame loop and the two clocks.
//! 2. **Presentation effects** (default: do nothing): small writes the original
//!    makes on DOM it owns and that the host reflects (the container background,
//!    the published layout facts, the loading affordance, the scroll-to-realtime
//!    button's state, the attribution mark, the ARIA announcement).
//! 3. **Legend feed** (default: do nothing): the original pushes legend state into
//!    its `InputsUI` DOM; here the same facts go through these calls and the legend
//!    DOM belongs to the host.
//!
//! The host is called WHILE the renderer is mid-operation (its state is borrowed),
//! so a host must not call back into the renderer from inside any of these methods:
//! queue the work and run it from the next event or frame instead. Two timing rules
//! follow: the task given to [`NativeHost::request_frame`] must run later, never from
//! inside `request_frame`, and [`WallClock::on_tick`] must not tick synchronously.
//! Subscribers of the renderer's events (crosshair, click, viewport, ...) are not
//! affected: they run after the operation, and may call back in freely.

use std::rc::Rc;

use crate::model::Inputs;
use crate::ports::chart_renderer::{
    IndicatorStatus, LayoutMode, RendererRootAttributes, VelaTheme, WallClock,
};
use crate::render::canvas::{Canvas2d, CanvasImage};
use crate::render::chrome::marks_paint::MarkIcons;
use crate::render::core::scheduler::FrameTask;
use crate::render::drawings::indicator_slices::SliceSurface;
use crate::render::drawings::user_controller::DrawingsUiEvent;
use crate::render::gl::Gl;

/// Which canvas of the pile. The pile, back to front: `Backdrop`, the below-data
/// `Layer`s, `Data`, `Volume`, `Vpvr`, the above-data `Layer`s, `Chrome`,
/// `Drawings`, `Cursor`. Only `Data` takes pointer input in a browser; the rest
/// are pointer-transparent, absolutely placed over the whole plot, and pinned
/// left to right whatever the page direction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CanvasSlot {
    /// Session highlights and gridlines: the very bottom of the pile.
    Backdrop,
    /// The geometry canvas the backend paints on (the one that takes gestures).
    Data,
    /// Bottom-anchored volume columns.
    Volume,
    /// The visible-range volume profile.
    Vpvr,
    /// Axes, price line, trade markers, the mark lane.
    Chrome,
    /// The user drawings' own layer.
    Drawings,
    /// The crosshair.
    Cursor,
    /// An SDK renderer layer (or a dedicated instance layer), by channel.
    Layer(String),
}

/// One canvas of the pile as the host owns it.
pub trait HostCanvas {
    /// The 2D context to paint on.
    fn context(&self) -> Rc<dyn Canvas2d>;
    /// The opaque handle `draw_image` composites (screenshots).
    fn image(&self) -> CanvasImage;
    /// Resize to `backing_width` x `backing_height` DEVICE pixels (which clears
    /// the canvas, as assigning `canvas.width` does) and display it at
    /// `css_width` x `css_height` CSS px: the explicit CSS size derived from the
    /// backing store, so nothing is rescaled at composite time.
    fn set_size(&self, backing_width: f64, backing_height: f64, css_width: f64, css_height: f64);
}

/// The data canvas as the host owns it when it paints with WebGL2 (see
/// [`NativeHost::create_gl_canvas`]).
pub trait HostGlCanvas {
    /// The canvas's WebGL2 context. The host requests it with the attributes the
    /// original's `mount` does (`alpha: true, antialias: false, depth: false,
    /// stencil: false, premultipliedAlpha: true`; the last one is required).
    fn gl(&self) -> Rc<dyn Gl>;
    /// The opaque handle `draw_image` composites (screenshots). The shell paints
    /// synchronously just before the screenshot, so a non-preserved drawing buffer
    /// still holds the frame.
    fn image(&self) -> CanvasImage;
    /// As [`HostCanvas::set_size`].
    fn set_size(&self, backing_width: f64, backing_height: f64, css_width: f64, css_height: f64);
}

/// The plot geometry the host reports (the values `syncSize` reads in the
/// original).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostMetrics {
    /// The wrapper's client size in CSS px. A non-positive side makes
    /// `resize` a no-op (the chart is not laid out yet).
    pub client_width: f64,
    pub client_height: f64,
    /// The plot box's real (fractional) size in CSS px: the wrapper minus the
    /// toolbar gutter the shell published through [`NativeHost::publish_layout`].
    pub plot_width: f64,
    pub plot_height: f64,
    /// `devicePixelRatio` (a non-positive value reads as 1).
    pub dpr: f64,
    /// The plot's size in INTEGER device pixels as the browser reports it
    /// (`devicePixelContentBoxSize`), when it describes the current box.
    /// `None` rounds `plot_width * dpr` instead.
    pub device_size: Option<(f64, f64)>,
}

/// Layout facts the original publishes as CSS variables on the mount container
/// (`--vela-toolbar-gutter`, `--vela-scale-gutter`, `--vela-bottom-gutter`,
/// `--vela-price-pane-top`, `--vela-price-pane-bottom`) and the plot's left
/// inset. Host overlays (status line, watermark, attribution) anchor to them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayoutFacts {
    /// Left gutter of the docked drawings toolbar, px. The host insets the plot
    /// by this much from the left edge.
    pub toolbar_gutter: f64,
    /// The full price-scale width incl. merged own-scale columns, px.
    pub scale_gutter: f64,
    /// Time axis plus collapsed strips below the lowest open pane, px.
    /// `None` until the first layout.
    pub bottom_gutter: Option<f64>,
    /// The price pane's vertical insets, px. `None` until the first layout.
    pub price_pane_top: Option<f64>,
    pub price_pane_bottom: Option<f64>,
}

/// The state of the scroll-to-realtime button (the button element is the
/// host's; the decision logic is the shell's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollButtonState {
    /// Show the button now.
    pub visible: bool,
    /// Distance from the plot's bottom edge, px.
    pub bottom_px: f64,
    /// Distance from the plot's right edge, px.
    pub right_px: f64,
    /// The touch variant is in force (mobile layout): a larger target.
    pub touch: bool,
}

/// The attribution mark's state. The license requires the host to show the mark
/// (or an equivalent) whenever `enabled`.
#[derive(Clone, Debug, PartialEq)]
pub struct AttributionState {
    /// `false` hides it.
    pub enabled: bool,
    /// A host-supplied mark shown INSTEAD of the built-in one (`attribution:
    /// '<html>'`); `None` is the built-in mark.
    pub html: Option<String>,
    /// Left offset px (toolbar gutter + 12).
    pub left_px: f64,
    /// Bottom offset px: above the time axis and any collapsed strips.
    pub bottom_px: f64,
    /// The ink color for the current plot background.
    pub ink: String,
}

/// What a host popover has to do for the timeline-mark popup (the popover
/// element is the host's).
#[derive(Clone, Debug, PartialEq)]
pub enum MarkPopoverAction {
    /// Open the popup for the cluster `key` anchored on a glyph.
    Open {
        key: String,
        mark_ids: Vec<String>,
        x: f64,
        y: f64,
        size: f64,
    },
    /// Close it (the cluster is gone, or the marks were hidden or replaced).
    Close,
    /// Keep it on its glyph after a chrome frame; `None` closes it (the glyph
    /// is gone).
    Track(Option<(f64, f64, f64)>),
}

/// One plot value beside a legend title.
#[derive(Clone, Debug, PartialEq)]
pub struct LegendPlotValue {
    pub value: String,
    pub color: String,
}

/// One legend row to create or refresh (`InputsUI.upsert`).
#[derive(Clone, Debug, PartialEq)]
pub struct LegendRow {
    pub id: String,
    /// The compact name the legend chip shows.
    pub title: String,
    pub pane_id: Option<String>,
    /// A core-computed native indicator.
    pub native: bool,
}

/// The host of a native renderer. See the module documentation.
pub trait NativeHost {
    // ── geometry ──

    /// The plot geometry now. Called by `resize` (and so by every layout).
    fn metrics(&self) -> HostMetrics;

    // ── the canvas pile ──

    /// Create the canvas for `slot` and put it in the plot (stacking order per the
    /// [`CanvasSlot`] doc; a `Layer` is appended and re-slotted by the next
    /// [`order_layer_canvases`](Self::order_layer_canvases)). Called at mount, and
    /// later for the dedicated layer of a mounted native indicator.
    fn create_canvas(&self, slot: &CanvasSlot) -> Rc<dyn HostCanvas>;
    /// Offer a WebGL2 canvas for [`CanvasSlot::Data`], instead of a 2D one. Called at
    /// mount, before `create_canvas(Data)`, when the backend mode allows WebGL2 (`auto`
    /// or `webgl2`). The default (`None`) is "no WebGL2 here" (the original's
    /// `supportsWebGL2()` probe answering false) and keeps the canvas2d backend.
    ///
    /// When the shell uses the canvas, the host owns it like any other of the pile
    /// (stacking position of the `Data` slot, sizing through
    /// [`HostGlCanvas::set_size`], pointer input, `remove_canvas(Data)` at destroy).
    /// When the shell cannot use it (the program does not compile or link) it calls
    /// `remove_canvas(Data)` and then `create_canvas(Data)`: the host must now hand out
    /// a 2D canvas (a canvas that has had a `webgl2` context cannot give a `2d` one).
    /// The same replacement happens when a context restore fails; the host re-attaches
    /// its pointer listeners to the new `Data` canvas.
    ///
    /// The host reports context loss and restoration through
    /// [`NativeRenderer::gl_context_lost`](super::NativeRenderer::gl_context_lost) and
    /// [`NativeRenderer::gl_context_restored`](super::NativeRenderer::gl_context_restored),
    /// calling `preventDefault()` on the `webglcontextlost` event so the context stays
    /// recoverable, and not from inside a host call.
    fn create_gl_canvas(&self) -> Option<Rc<dyn HostGlCanvas>> {
        None
    }
    /// Remove a canvas (a dedicated layer of a removed indicator, or everything at
    /// destroy).
    fn remove_canvas(&self, slot: &CanvasSlot);
    /// Re-slot the SDK layer canvases: `below` go under the data canvas, `above`
    /// over the vpvr canvas (under the chrome), each back to front, by channel.
    /// Called only when the order changed.
    fn order_layer_canvases(&self, below: &[String], above: &[String]);
    /// An offscreen canvas for a drawing slice (script drawings and tables
    /// prepainted into the series stack).
    fn create_slice_surface(&self) -> Rc<dyn SliceSurface>;
    /// Registry icons as canvas images for the mark lane. `None`: icon tokens
    /// paint bare. When an image lands, call
    /// [`NativeRenderer::invalidate_chrome`](super::NativeRenderer::invalidate_chrome).
    fn mark_icons(&self) -> Option<Rc<dyn MarkIcons>> {
        None
    }

    // ── frame loop and clocks ──

    /// `requestAnimationFrame`: run `task` on the next frame and return a handle
    /// for [`cancel_frame`](Self::cancel_frame). Must never run `task`
    /// synchronously.
    fn request_frame(&self, task: FrameTask) -> u32;
    /// `cancelAnimationFrame`.
    fn cancel_frame(&self, handle: u32);
    /// `performance.now()` (falling back to `Date.now()`): the frame clock.
    fn frame_now_ms(&self) -> f64;
    /// `Date.now()`: wall-clock epoch ms (the bar-close countdown).
    fn wall_ms(&self) -> f64;
    /// The renderer's own second-aligned pulse (the original's `SecondClock`),
    /// used for the countdown chip when no host clock was set through
    /// `set_wall_clock`. `None`: the chip then updates only on other repaints.
    fn second_clock(&self) -> Option<Rc<dyn WallClock>> {
        None
    }

    // ── presentation effects ──

    /// Paint the container background (`wrapper.style.background`): the live
    /// plot background.
    fn set_background(&self, _css_color: &str) {}
    /// The chrome theme (the live theme pinned to the stable app surface) the
    /// host's DOM chrome (toolbar, dialogs, legend, buttons) paints with
    /// (`applyChromeTokens`). Called at mount, `set_theme` and `apply_config`.
    fn apply_chrome_theme(&self, _theme: &VelaTheme) {}
    /// The `lang` / `dir` attributes of the root element.
    fn apply_root_attributes(&self, _attrs: &RendererRootAttributes) {}
    /// The layout facts changed (see [`LayoutFacts`]).
    fn publish_layout(&self, _layout: &LayoutFacts) {}
    /// The loading affordance (three pulsing dots over the plot) goes up or down.
    /// A host builds it fresh per show so it picks up the current theme.
    fn set_loading_visible(&self, _visible: bool) {}
    /// The scroll-to-realtime button changed (see [`ScrollButtonState`]).
    fn scroll_button(&self, _state: &ScrollButtonState) {}
    /// The attribution mark changed (see [`AttributionState`]).
    fn attribution(&self, _state: &AttributionState) {}
    /// Timeline-mark popup directives.
    fn mark_popover(&self, _action: &MarkPopoverAction) {}
    /// The ARIA live-region text for the focused bar (keyboard navigation).
    fn announce(&self, _text: &str) {}
    /// The geometry of the `shadeRight` veil painted on the cursor canvas
    /// (`x`, `width`, `height` px), or `None` when no veil is painted: the host's
    /// styleable twin element follows it.
    fn shade_right(&self, _area: Option<(f64, f64, f64)>) {}
    /// The `keyboard` feature changed: make the data canvas focusable (with the
    /// application role and the help label) or not.
    fn set_keyboard_enabled(&self, _enabled: bool) {}
    /// Move keyboard focus onto the data canvas.
    fn focus(&self) {}

    /// A diagnostic for the developer (`console.warn`).
    fn warn(&self, _message: &str) {}
    /// A feature that only toggles a DOM widget changed (`settings`,
    /// `indicatorTitles`, `indicatorValues`, `dialogHost`): the shell keeps the value
    /// (and reads it back); the host shows or hides the widget.
    fn feature_changed(&self, _key: &str, _value: &serde_json::Value) {}
    /// The host shell's chrome size class changed.
    fn set_layout_mode(&self, _mode: LayoutMode) {}

    // ── dialogs (the dialog DOM is the host's) ──

    /// Whether the host provides the in-chart settings dialogs (gates
    /// `supports_open_indicator_settings`).
    fn has_dialogs(&self) -> bool {
        false
    }
    /// Open (or toggle) the chart settings dialog, landing on `section`.
    fn open_settings_dialog(&self, _section: Option<&str>) {}
    /// Open one indicator's settings dialog (a silent no-op for an unknown id).
    fn open_indicator_settings(&self, _indicator_id: &str) {}
    /// Close any in-chart dialog that is open.
    fn close_dialogs(&self) {}
    /// The host settings sections or the visibility policy changed; read them back
    /// through `NativeRenderer::settings_sections` / `hidden_settings`.
    fn settings_changed(&self) {}

    // ── user drawings UI (the toolbar, settings popup, inline editor and menus are the host's) ──

    /// The user-drawings controller reports a change of the DOM widgets it drives: the
    /// docked toolbar, the settings popup, the inline text editor, the magnifier's timeframe
    /// menu. See [`DrawingsUiEvent`] for what the browser does with each; the answers come
    /// back through the public methods of
    /// [`UserDrawingController`](crate::render::drawings::UserDrawingController). Like the
    /// rest of the seam, called possibly under the renderer's borrow: never call back
    /// synchronously.
    fn drawings_ui(&self, _event: &DrawingsUiEvent) {}

    // ── legend feed (the legend DOM is the host's) ──

    /// A legend row was created or refreshed (`InputsUI.upsert`); the input schema
    /// and values are read back from the model through
    /// [`NativeRenderer::indicator_model`](super::NativeRenderer::indicator_model).
    fn legend_upsert(&self, _row: &LegendRow) {}
    fn legend_remove(&self, _id: &str) {}
    /// Programmatic input values changed (`set_indicator_inputs`); `props` refreshes
    /// the declaration-prop values the same way.
    fn legend_set_values(&self, _id: &str, _values: &Inputs, _props: Option<&Inputs>) {}
    fn legend_set_status(&self, _id: &str, _status: IndicatorStatus) {}
    fn legend_set_pane(&self, _id: &str, _pane_id: &str) {}
    fn legend_set_visible(&self, _id: &str, _visible: bool) {}
    /// The legend row titles were re-read in a new language.
    fn legend_relabel(&self, _titles: &[(String, String)]) {}
    /// The indicators' paint order (back to front), by indicator id; the legend lists the
    /// front-most first.
    fn legend_order(&self, _ids: &[String]) {}
    /// Which indicator fronts each collapsed pane's strip (`None`: no legend row
    /// can), keyed by pane id.
    fn legend_collapsed_panes(&self, _masters: &[(String, Option<String>)]) {}
    /// The plot values beside every legend title, per indicator id (an indicator
    /// absent from the list, hidden, has its readout cleared). Pushed after every
    /// frame; the host diffs.
    fn legend_plot_values(&self, _values: &[(String, Vec<LegendPlotValue>)]) {}
    /// The pane layout changed (bounds, collapse, order, maximize): hosts that
    /// pin DOM to pane tops (legends, pane controls, axis buttons) reposition.
    fn layout_changed(&self) {}
    /// A view of what the original's `setLegendActions` / callouts / overview
    /// action provide changed; the providers themselves are held by the shell and
    /// read through the renderer. Present for hosts that rebuild the legend.
    fn legend_providers_changed(&self) {}
}
