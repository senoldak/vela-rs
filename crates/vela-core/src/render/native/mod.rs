//! The native renderer shell, ported from the original
//! `src/renderers/native/NativeRenderer.ts`: the object that owns the scene and the
//! coordinate system, lays out the panes, computes the scales, runs the viewport
//! (pan, zoom, fling, eased scroll), drives the frame loop, calls the painters in
//! order and implements the chart-renderer port ([`ChartRenderer`]).
//!
//! # Shape
//!
//! `vela-core` has no DOM, so the shell is written against one small host seam,
//! [`NativeHost`], which the browser crate implements (the in-memory reference
//! implementation is [`testing::TestHost`]). The host owns the canvases of the pile
//! (the shell asks it to create, size, order and remove them and paints on their
//! [`Canvas2d`](crate::render::canvas::Canvas2d) contexts), the frame loop and the
//! clocks, and the presentation effects the original applies to DOM it owns.
//!
//! [`NativeRenderer`] is shared as `Rc<dyn ChartRenderer>` (every port method takes
//! `&self`): [`NativeRenderer::new`] returns an `Rc<NativeRenderer>`, which coerces
//! to the trait object `Vela` holds. All state lives in one struct behind a
//! `RefCell`; an operation runs under the borrow, and whatever it owes subscribers
//! (crosshair, click, viewport, pane-action, mark-click, config-changed events) is
//! queued and delivered AFTER the borrow is released, so a subscriber may call
//! straight back into the renderer. The one synchronous callback is
//! `on_price_style_will_change`, which the port promises to run before the scene is
//! touched. The host seam is called WHILE the borrow is held: a host must not call
//! back into the renderer from inside one of its [`NativeHost`] methods (queue the
//! work instead).
//!
//! # Gesture entry points
//!
//! The original's `InputController` and `KeyboardController` are DOM listeners; the
//! browser crate keeps them and wires its events to the public gesture methods on
//! [`NativeRenderer`] (`pointer_move`, `click`, `zoom_to`, `fling`, `apply_viewport`,
//! `begin_price_scale`, ..., `handle_key`), with the pure decisions in
//! [`input_logic`](crate::render::core::input_logic).
//!
//! # Left for the browser crate and later stages
//!
//! - DOM widgets and their element code: `SettingsDialog` (and the settings gear),
//!   `InputsUI` (the legend rows and the indicator-inputs dialog), `PaneControls`,
//!   `AxisScaleButtons`, `LabelTooltip`, `MarkPopover`, the scroll-to-realtime
//!   button, the attribution element, the loading element, the ARIA live region, the
//!   `shadeRight` twin element. The shell keeps their LOGIC and publishes the facts
//!   they need through [`NativeHost`] (legend feed, [`LayoutFacts`],
//!   [`ScrollButtonState`], [`AttributionState`], [`MarkPopoverAction`], `announce`,
//!   `shade_right`) and through pull methods (`pane_views`, `axis_scale_views`,
//!   `legend_values`, `overlay_stack`, ...).
//! - `screenshot` / `screenshotCanvas`: the DOM-overlay rasterization and the
//!   offscreen target. [`NativeRenderer::screenshot_pile`] runs the fresh
//!   synchronous paint and returns the canvases in compositing order; the host
//!   composes them (`draw_image`) and encodes. The port's `screenshot` therefore
//!   keeps its "not supported" default.
//! - `UserDrawingController` / `DrawingInteraction` / the drawing toolbar and panels:
//!   plug in through [`UserDrawingsHook`] and
//!   [`NativeRenderer::set_user_drawings`]; without one `user_drawings_port()` is
//!   `None` and the frame skips the drawings layers.
//! - The WebGL2 backend runs when the host offers a GL canvas
//!   ([`NativeHost::create_gl_canvas`], default `None`) and the backend mode (`auto` or
//!   `webgl2`) allows it; otherwise, or when its program does not build, or when a
//!   context restore fails, the shell uses `Canvas2dBackend`. The `glow` feature drives
//!   its bloom pass. The workspace's `maxWebglCells` policy is
//!   [`backend_mode_for_cells`](crate::render::backend::webgl2_backend::backend_mode_for_cells).
//! - Features that only toggle DOM widgets keep their stored value and read back (so
//!   config and feature round-trips work), and the host is told through
//!   [`NativeHost::feature_changed`]: `settings`, `indicatorTitles`,
//!   `indicatorValues`, `dialogHost`, `keyboard`.
//! - The `priceAxisTicks` feature carries a function: set it with
//!   [`NativeRenderer::set_price_axis_ticks`] (through `apply_feature` a `null` or
//!   `false` clears it and any other value refreshes the memoized ticks); a custom
//!   `freePan` trigger likewise goes through [`NativeRenderer::set_free_pan`].
//! - The dedicated settings, legend-provider and symbol-picker plumbing is held by
//!   the shell and exposed for the host's dialogs (`legend_actions_provider`,
//!   `legend_callouts_provider`, `legend_overview_action`, `symbol_picker`,
//!   `settings_sections`, `hidden_settings`).

mod features;
mod frame;
mod host;
mod input;
mod lifecycle;
mod panes;
mod readout;
mod scales;
mod state;
mod subs;
pub mod testing;
#[cfg(test)]
mod tests;
mod user_drawings;
mod util;
mod viewport;

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};

use crate::chart_types::ChartTypeRegistry;
use crate::drawings::SnapMode;
use crate::i18n::Translator;
use crate::marks::{MarkClickEvent, MarkGroup, TimelineMark};
use crate::model::{IndicatorModel, Millis, Ohlcv, Pane, PaneKind, ScenePatch};
use crate::options::{
    FreePanTrigger, PriceAxisTicksFn, RendererConstructor, RendererDisplayOptions,
};
use crate::ports::chart_renderer::{
    AxisLongPressEvent, ChartRenderer, ClickEvent, CrosshairEvent, DataWindowReadout,
    DrawingsRendererPort, IndicatorRenderHandle, IndicatorStatus, InputChangeEvent, LayoutMode,
    LegendActionProvider, LegendCalloutProvider, Listener, MoveIndicatorListener, MoveTarget,
    PaneAction, PaneMoveDir, PriceStyle, PriceStyleWillChangeListener, RendererCapabilities,
    RendererRootAttributes, SetBarsOptions, SettingsSection, SettingsVisibility, SymbolPickerFn,
    ThemeId, ToggleVisibleListener, VelaTheme, VisibleRange, VisibleRangeOptions, WallClock,
};
use crate::ports::market_data::SymbolFacts;
use crate::render::backend::capabilities::NATIVE_CAPABILITIES;
use crate::render::backend::layers::LayerRegistry;
use crate::render::backend::render_backend::{BackendKind, RenderBackend};
use crate::render::canvas::CanvasImage;
use crate::render::chrome::marks_layout::effective_mark_groups;
use crate::render::chrome::settings_visibility::{
    HostRowIdSource, HostSectionIdSource, settings_id_catalog,
};
use crate::render::core::chart_config::{
    CandleColors, ChartConfig, factory_reset_config, follow_theme_colors, price_style_ids,
};
use crate::render::core::input_logic::{
    InputSettings, KeyAction, KeyInput, drag_pan_right_offset, key_to_action,
};
use crate::render::core::overlay_stack::{
    Neighbor, StackEntry, layer_above, layer_below, lowered_z, price_stack, raised_z, untie,
};
use crate::render::core::scene_graph::{ScaleMode, pane_invert, pane_log_scale};
use crate::render::core::scheduler::{
    CancelFrame, InvalidateLevel, OnFrame, RequestFrame, Scheduler,
};
use crate::render::core::{
    Animator, CoordinateSystem, ViewportState,
    animator::{Now, TickFn},
};
use crate::render::drawings::UserDrawingController;
use crate::runtime::Unsubscribe;

pub use host::{
    AttributionState, CanvasSlot, HostCanvas, HostGlCanvas, HostMetrics, LayoutFacts,
    LegendPlotValue, LegendRow, MarkPopoverAction, NativeHost, ScrollButtonState,
};
pub use user_drawings::{FrameView, UserDrawingsHook, with_projector};

use state::{Event, PRICE_PANE_ID, State};
use subs::Subs;

/// What a pane looks like to the host's pane controls and axis buttons.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneView {
    pub id: String,
    pub kind: PaneKind,
    pub collapsed: bool,
    pub maximized: bool,
    pub top: f64,
    pub height: f64,
    /// Position in the top-to-bottom order.
    pub index: usize,
    pub count: usize,
    /// The pane's scale is autoscaled (no frozen manual window).
    pub auto: bool,
    pub log: bool,
    pub invert: bool,
    pub mode: ScaleMode,
}

/// The look the price series ACTUALLY paints with (what series-mirroring content,
/// such as the magnifier inset, needs).
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesLook {
    pub style: String,
    pub up_color: String,
    pub down_color: String,
    pub line_color: String,
}

/// The canvases of a screenshot in compositing order, with the background to fill
/// first. Only the crosshair canvas is excluded.
pub struct ScreenshotPile {
    pub background: String,
    /// Backing size in device px.
    pub width: f64,
    pub height: f64,
    /// Back to front: the pile (backdrop, layers, data, volume, vpvr, layers), the
    /// chrome, then the drawings canvas.
    pub images: Vec<CanvasImage>,
}

/// What a [`NativeRenderer`] is built from besides its host.
#[derive(Clone)]
pub struct NativeParts {
    /// The chart-type registry (shared with the engine).
    pub chart_types: Rc<RefCell<ChartTypeRegistry>>,
    /// The registered SDK renderer layers.
    pub layers: LayerRegistry,
}

impl Default for NativeParts {
    fn default() -> Self {
        Self {
            chart_types: Rc::new(RefCell::new(ChartTypeRegistry::builtin())),
            layers: LayerRegistry::new(),
        }
    }
}

/// The native renderer. See the module documentation.
#[allow(clippy::type_complexity)] // one subscriber list per port event
pub struct NativeRenderer {
    st: RefCell<State>,
    host: Rc<dyn NativeHost>,
    sched: Rc<Scheduler>,
    viewport_cbs: Rc<Subs<dyn FnMut(&VisibleRange)>>,
    crosshair_cbs: Rc<Subs<dyn FnMut(&CrosshairEvent)>>,
    click_cbs: Rc<Subs<dyn FnMut(&ClickEvent)>>,
    axis_long_press_cbs: Rc<Subs<dyn FnMut(&AxisLongPressEvent)>>,
    pane_action_cbs: Rc<Subs<dyn FnMut(&PaneAction)>>,
    mark_click_cbs: Rc<Subs<dyn FnMut(&MarkClickEvent)>>,
    config_changed_cbs: Rc<Subs<dyn FnMut()>>,
    chart_type_settings_cbs: Rc<Subs<dyn FnMut(&str, &Map<String, Value>)>>,
    theme_select_cbs: Rc<Subs<dyn FnMut(&ThemeId)>>,
    input_change_cbs: Rc<Subs<dyn FnMut(&InputChangeEvent)>>,
    remove_indicator_cbs: Rc<Subs<dyn FnMut(&String)>>,
    toggle_visible_cbs: Rc<Subs<dyn FnMut(&str, bool)>>,
    move_indicator_cbs: Rc<Subs<dyn FnMut(&str, &MoveTarget)>>,
    price_style_cbs: Rc<Subs<dyn FnMut(&PriceStyle)>>,
    price_style_will_cbs: Rc<Subs<dyn FnMut(&PriceStyle, &PriceStyle)>>,
    user_port: RefCell<Option<Rc<dyn DrawingsRendererPort>>>,
    /// The default user-drawings layer, while it is the one attached.
    drawings_ctl: RefCell<Option<Rc<UserDrawingController>>>,
    symbol_picker: RefCell<Option<SymbolPickerFn>>,
    legend_actions: RefCell<Option<LegendActionProvider>>,
    legend_callouts: RefCell<Option<LegendCalloutProvider>>,
    legend_overview: RefCell<Option<Rc<dyn Fn()>>>,
    settings_sections: RefCell<Vec<SettingsSection>>,
}

impl NativeRenderer {
    /// A renderer on `host`, with the built-in chart types and no SDK layers. Hand it
    /// to `Vela` as `Rc<dyn ChartRenderer>` (it coerces): `let renderer: Rc<dyn
    /// ChartRenderer> = NativeRenderer::new(host, Some(display_options));`.
    pub fn new(host: Rc<dyn NativeHost>, display: Option<RendererDisplayOptions>) -> Rc<Self> {
        Self::with_parts(host, display, NativeParts::default())
    }

    /// As [`new`](Self::new), with the engine's chart-type registry and the SDK layers.
    pub fn with_parts(
        host: Rc<dyn NativeHost>,
        display: Option<RendererDisplayOptions>,
        parts: NativeParts,
    ) -> Rc<Self> {
        Rc::new_cyclic(|me: &std::rc::Weak<NativeRenderer>| {
            let request: RequestFrame = {
                let h = host.clone();
                Rc::new(move |task| h.request_frame(task))
            };
            let cancel: CancelFrame = {
                let h = host.clone();
                Rc::new(move |handle| h.cancel_frame(handle))
            };
            let on_frame: OnFrame = {
                let me = me.clone();
                Rc::new(move |level| {
                    if let Some(r) = me.upgrade() {
                        r.with(|st| st.render_frame(level));
                    }
                })
            };
            let sched = Rc::new(Scheduler::new(on_frame, request.clone(), cancel.clone()));
            let tick: TickFn = {
                let me = me.clone();
                Rc::new(move |dt| match me.upgrade() {
                    Some(r) => r.with(|st| st.anim_tick(dt)),
                    None => false,
                })
            };
            let now: Now = {
                let h = host.clone();
                Rc::new(move || h.frame_now_ms())
            };
            let anim = Rc::new(Animator::new(tick, request, cancel, now));
            let state = State::new(
                me.clone(),
                host.clone(),
                sched.clone(),
                anim,
                parts.chart_types,
                parts.layers,
                display,
            );
            // The user-drawings layer is part of the renderer, as in the original (its
            // `mount` creates the controller): attached here, replaceable through
            // `set_user_drawings`.
            let drawings =
                crate::render::drawings::UserDrawingController::new(host.clone(), me.clone());
            let mut state = state;
            state.user_drawings = Some(drawings.clone());
            NativeRenderer {
                st: RefCell::new(state),
                host,
                sched,
                viewport_cbs: Subs::new(),
                crosshair_cbs: Subs::new(),
                click_cbs: Subs::new(),
                axis_long_press_cbs: Subs::new(),
                pane_action_cbs: Subs::new(),
                mark_click_cbs: Subs::new(),
                config_changed_cbs: Subs::new(),
                chart_type_settings_cbs: Subs::new(),
                theme_select_cbs: Subs::new(),
                input_change_cbs: Subs::new(),
                remove_indicator_cbs: Subs::new(),
                toggle_visible_cbs: Subs::new(),
                move_indicator_cbs: Subs::new(),
                price_style_cbs: Subs::new(),
                price_style_will_cbs: Subs::new(),
                user_port: RefCell::new(Some(drawings.clone())),
                drawings_ctl: RefCell::new(Some(drawings)),
                symbol_picker: RefCell::new(None),
                legend_actions: RefCell::new(None),
                legend_callouts: RefCell::new(None),
                legend_overview: RefCell::new(None),
                settings_sections: RefCell::new(Vec::new()),
            }
        })
    }

    /// The `RendererConstructor` a `VelaOptions` carries: a closure that builds a
    /// renderer on `host` from the display options the chart resolved.
    pub fn constructor(host: Rc<dyn NativeHost>, parts: NativeParts) -> RendererConstructor {
        Rc::new(move |display| {
            let r: Rc<dyn ChartRenderer> =
                NativeRenderer::with_parts(host.clone(), Some(display), parts.clone());
            r
        })
    }

    // ── plumbing ──

    /// Run one operation under the state borrow, then deliver what it queued.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let r = f(&mut self.st.borrow_mut());
        self.drain();
        r
    }

    fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.st.borrow())
    }

    /// Deliver queued events (subscribers) and the synchronous flush request, with
    /// the state released.
    fn drain(&self) {
        loop {
            let (events, flush) = {
                let mut st = self.st.borrow_mut();
                (std::mem::take(&mut st.outbox), st.flush.take())
            };
            if events.is_empty() && flush.is_none() {
                break;
            }
            for event in events {
                self.deliver(event);
            }
            if let Some(level) = flush {
                self.sched.flush_now(level);
            }
        }
    }

    fn deliver(&self, event: Event) {
        match event {
            Event::Viewport(range) => {
                for cb in self.viewport_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&range);
                    }
                }
            }
            Event::Crosshair(e) => {
                for cb in self.crosshair_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&e);
                    }
                }
            }
            Event::Click(e) => {
                for cb in self.click_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&e);
                    }
                }
            }
            Event::AxisLongPress(e) => {
                for cb in self.axis_long_press_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&e);
                    }
                }
            }
            Event::PaneAction(a) => {
                for cb in self.pane_action_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&a);
                    }
                }
            }
            Event::MarkClick(e) => {
                for cb in self.mark_click_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&e);
                    }
                }
            }
            Event::ConfigChanged => {
                for cb in self.config_changed_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)();
                    }
                }
            }
            Event::ChartTypeSettings(id, values) => {
                for cb in self.chart_type_settings_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&id, &values);
                    }
                }
            }
            Event::PriceStyle(style) => {
                for cb in self.price_style_cbs.snapshot() {
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        (**f)(&style);
                    }
                }
            }
        }
    }

    /// The wall-clock pulse of the countdown chip.
    pub(crate) fn countdown_tick(&self) {
        self.with(|st| {
            if st.scene.show_countdown && !st.scene.bars.is_empty() {
                st.invalidate(InvalidateLevel::Chrome);
            }
        });
    }

    pub(crate) fn intro_reveal_frame(&self, start: f64) {
        self.with(|st| st.intro_reveal_step(start));
    }

    pub(crate) fn intro_fade_frame(&self, start: f64) {
        self.with(|st| st.intro_fade_step(start));
    }

    pub(crate) fn mark_pulse_frame(&self) {
        self.with(|st| st.mark_pulse_step());
    }

    // ── the price style: the one write path ──

    /// THE single write path for the base price style at runtime (feature set,
    /// settings dialog, config template). Announces the switch while the scene still
    /// holds the old style (a listener may paint or capture that frame), then updates
    /// the scene and notifies the core. Announced only when what is DRAWN changes.
    fn show_price_style(&self, chosen: Option<String>) {
        let plan = self.read(|st| {
            let chosen = chosen.unwrap_or_else(|| st.chosen_style.clone());
            let registry = st.chart_types.borrow();
            let known = price_style_ids(&registry);
            let shown = crate::chart_types::shown_price_style(
                &registry,
                &chosen,
                st.symbol_facts.as_ref(),
                &known,
            );
            (chosen, shown, st.scene.price_style.clone())
        });
        let (chosen, shown, from) = plan;
        if shown == from {
            self.with(|st| st.chosen_style = chosen);
            return;
        }
        for cb in self.price_style_will_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(&from, &shown);
            }
        }
        self.with(|st| {
            st.chosen_style = chosen;
            st.scene.price_style = shown.clone();
            {
                let registry = st.chart_types.borrow();
                st.scene.base_painting =
                    crate::render::core::chart_config::base_painting_of(&shown, &registry);
                st.scene.candle_override = crate::render::core::chart_config::candle_override_for(
                    &shown,
                    &st.scene.style.chart_types,
                    &registry,
                );
            }
            st.emit(Event::PriceStyle(shown));
        });
    }

    // ── public extras: host wiring ──

    /// Plug the interactive user-drawings controller in. `hook` serves the shell's
    /// frame; `port` is what `user_drawings_port()` hands the core's
    /// `DrawingController`. `None` unplugs.
    pub fn set_user_drawings(
        &self,
        plug: Option<(Rc<dyn UserDrawingsHook>, Rc<dyn DrawingsRendererPort>)>,
    ) {
        let (hook, port) = match plug {
            Some((h, p)) => (Some(h), Some(p)),
            None => (None, None),
        };
        *self.user_port.borrow_mut() = port;
        *self.drawings_ctl.borrow_mut() = None;
        let old = self.with(|st| {
            if let Some(h) = &hook {
                h.set_history_chords(st.history_chords_enabled);
                h.set_layout_mode(st.layout_mode);
            }
            std::mem::replace(&mut st.user_drawings, hook)
        });
        if let Some(old) = old {
            old.destroy(); // the replaced (default) layer closes its popup and editor
        }
    }

    /// The default user-drawings controller the shell attaches at construction: the
    /// object the browser's pointer, keyboard, toolbar, popup and text-editor handlers call
    /// (see [`UserDrawingController`]). `None` once [`set_user_drawings`](Self::set_user_drawings)
    /// replaced it with another layer.
    pub fn user_drawings_controller(&self) -> Option<Rc<UserDrawingController>> {
        self.drawings_ctl.borrow().clone()
    }

    /// The `priceAxisTicks` feature with its function: `None` restores the built-in
    /// ticks. Setting it again, even the same function, drops the memoized ticks so a
    /// host can refresh them.
    pub fn set_price_axis_ticks(&self, hook: Option<PriceAxisTicksFn>) {
        self.with(|st| {
            st.price_axis_ticks.set_hook(hook);
            st.invalidate(InvalidateLevel::Full);
        });
    }

    /// The `freePan` feature with a trigger a feature value cannot carry (a custom
    /// predicate); `FreePanTrigger::Enabled(false)` turns it off.
    pub fn set_free_pan(&self, trigger: FreePanTrigger) {
        self.with(|st| st.apply_free_pan_trigger(trigger));
    }

    /// The host's own frame callback for an externally owned frame loop is not
    /// needed: the shell arms frames through [`NativeHost::request_frame`]. This
    /// repaints everything now, synchronously (used by tests and screenshots).
    pub fn flush(&self) {
        self.with(|st| st.flush = Some(InvalidateLevel::Full));
    }

    /// A mark icon landed (or any reason to repaint the chrome only).
    pub fn invalidate_chrome(&self) {
        self.with(|st| st.invalidate(InvalidateLevel::Chrome));
    }

    /// Ask for a data-tier repaint (the original's `requestScaleUpdate` /
    /// `requestDataPaint` of the drawings controller).
    pub fn invalidate_data(&self) {
        self.with(|st| st.invalidate(InvalidateLevel::Light));
    }

    /// The geometry backend in force: WebGL2 when the host offered a GL canvas the mode
    /// allowed and its program built, else canvas2d (also after a failed context restore).
    pub fn geometry_backend(&self) -> BackendKind {
        self.read(|st| st.backend.kind())
    }

    /// The host's data canvas lost its WebGL2 context (`webglcontextlost`; the host has
    /// called `preventDefault()` on the event). Painting pauses until the restore. A
    /// no-op under the canvas2d backend.
    pub fn gl_context_lost(&self) {
        self.with(|st| st.gl_context_lost());
    }

    /// The context came back (`webglcontextrestored`). The GL objects are rebuilt and the
    /// chart repaints; if they cannot be rebuilt, the shell replaces the data canvas with
    /// a 2D one (`remove_canvas(Data)`, then `create_canvas(Data)`) and falls back to
    /// canvas2d. A no-op under the canvas2d backend.
    pub fn gl_context_restored(&self) {
        self.with(|st| st.gl_context_restored());
    }

    /// The mark popup closed on the host's side (Escape, click away).
    pub fn mark_popover_closed(&self) {
        self.with(|st| {
            if st.mark_popover_key.take().is_some() {
                st.scene.marks_active_key = None;
                st.invalidate(InvalidateLevel::Chrome);
            }
        });
    }

    // ── public extras: the legend / dialog UIs raise these ──

    /// The user edited an input in the host's settings UI.
    pub fn notify_input_change(&self, event: &InputChangeEvent) {
        for cb in self.input_change_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(event);
            }
        }
    }

    /// The user removed an indicator in-chart (the legend close button).
    pub fn notify_remove_indicator(&self, id: &str) {
        let id = id.to_string();
        for cb in self.remove_indicator_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(&id);
            }
        }
    }

    /// The user toggled an indicator's visibility in-chart (the legend eye).
    pub fn notify_toggle_visible(&self, id: &str, visible: bool) {
        for cb in self.toggle_visible_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(id, visible);
            }
        }
    }

    /// The user asked to move an indicator to another pane (legend menu or drag).
    pub fn notify_move_indicator(&self, id: &str, target: &MoveTarget) {
        for cb in self.move_indicator_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(id, target);
            }
        }
    }

    /// The app theme in force (the stable surface theme, not the config-recolored plot).
    pub fn app_theme(&self) -> VelaTheme {
        self.read(|st| st.app_theme.clone())
    }

    /// The settings dialog's Theme row picked a theme (a built-in's name or a
    /// registered id): the host's core applies it.
    pub fn notify_theme_select(&self, theme: &str) {
        let theme = theme.to_string();
        for cb in self.theme_select_cbs.snapshot() {
            if let Ok(mut f) = cb.try_borrow_mut() {
                (**f)(&theme);
            }
        }
    }

    // ── public extras: gestures (what InputController / KeyboardController call) ──

    /// The pointer moved over the plot (plot-relative px).
    pub fn pointer_move(&self, x: f64, y: f64) {
        self.with(|st| st.handle_pointer_move(Some(x), Some(y)));
    }

    /// The pointer left the plot (or a touch released): the crosshair hides.
    pub fn pointer_leave(&self) {
        self.with(|st| st.handle_pointer_move(None, None));
    }

    /// A click on the plot. A lane glyph takes the click whole; otherwise the
    /// drawings layer deselects and the click is reported (`on_click`).
    pub fn click(&self, x: f64, y: f64) {
        let hook = self.with(|st| {
            if st.handle_mark_click(x, y) {
                return None;
            }
            Some(st.user_drawings.clone())
        });
        let Some(hook) = hook else { return };
        if let Some(h) = hook {
            h.deselect(); // a click on the empty plot ends a (multi-)selection
        }
        self.with(|st| st.handle_click(x, y));
    }

    /// A double-click (or double-tap) inside the data area: maximize the pane under
    /// it, or restore.
    pub fn data_double_click(&self, _x: f64, y: f64) {
        self.with(|st| st.data_dbl_click(y));
    }

    /// Instant viewport set (drag, freeze-on-touch, `setVisibleRange`): stops any
    /// animation.
    pub fn apply_viewport(&self, vp: ViewportState) {
        self.with(|st| st.apply_viewport(vp));
    }

    /// Pan by `dx` pixels from the current viewport (a drag right reveals earlier
    /// bars): `apply_viewport` over [`drag_pan_right_offset`].
    pub fn pan_by_pixels(&self, dx: f64) {
        self.with(|st| {
            let vp = st.viewport();
            let ro = drag_pan_right_offset(vp.right_offset, dx, st.coords.px_per_bar());
            st.apply_viewport(ViewportState {
                bar_spacing: vp.bar_spacing,
                right_offset: ro,
            });
        });
    }

    /// Eased cursor-anchored zoom toward `target` bar spacing, pinning
    /// `anchor_logical` at pixel `anchor_x`.
    pub fn zoom_to(&self, target: f64, anchor_logical: f64, anchor_x: f64) {
        self.with(|st| st.zoom_to(target, anchor_logical, anchor_x));
    }

    /// Wheel zoom: scale the bar spacing by `factor` with the anchor chosen by
    /// `zoomAnchor` (right edge by default, else the bar under `cursor_x`).
    pub fn wheel_zoom(&self, factor: f64, cursor_x: f64) {
        self.with(|st| {
            let anchor = crate::render::core::input_logic::wheel_zoom_anchor(
                &st.coords,
                cursor_x,
                st.zoom_anchor_right,
            );
            let target = st.viewport().bar_spacing * factor;
            st.zoom_to(target, anchor.logical, anchor.x);
        });
    }

    /// Inertial pan with a rightOffset velocity (logical units / ms) that decays.
    pub fn fling(&self, velocity: f64) {
        self.with(|st| st.fling(velocity));
    }

    /// Grab the price axis at plot px `(x, y)`: freezes that scale into manual mode.
    pub fn begin_price_scale(&self, x: f64, y: f64) {
        self.with(|st| st.begin_price_scale(x, y));
    }

    /// Rescale the grabbed scale around its center by the total drag `dy`.
    pub fn price_scale_by(&self, dy_total: f64) {
        self.with(|st| st.price_scale_by(dy_total));
    }

    /// Grab inside the data area for a vertical price pan; true when available.
    pub fn begin_price_pan(&self, y: f64, unlock: bool) -> bool {
        self.with(|st| st.begin_price_pan(y, unlock))
    }

    /// Pan the grabbed pane's price window by the total drag `dy`.
    pub fn price_pan_by(&self, dy_total: f64) {
        self.with(|st| st.price_pan_by(dy_total));
    }

    /// Double-click the price axis: drop manual mode for that scale.
    pub fn reset_price_scale(&self, x: f64, y: f64) {
        self.with(|st| st.reset_price_scale(x, y));
    }

    /// Whether `y` is within the hit zone of a draggable pane separator.
    pub fn pane_separator_at(&self, y: f64) -> bool {
        self.read(|st| st.pane_separator_at(y))
    }

    pub fn begin_pane_resize(&self, y: f64) {
        self.with(|st| st.begin_pane_resize(y));
    }

    pub fn pane_resize_by(&self, dy_total: f64) {
        self.with(|st| st.pane_resize_by(dy_total));
    }

    /// Release the separator: reports the resize once if it changed anything.
    pub fn end_pane_resize(&self) {
        self.with(|st| st.end_pane_resize());
    }

    /// Double-click a separator: split the two adjacent panes evenly.
    pub fn reset_pane_size(&self, y: f64) {
        self.with(|st| st.reset_pane_size(y));
    }

    /// Re-fit the view and drop every manual scale (keyboard `0`).
    pub fn reset_view(&self) {
        self.with(|st| st.reset_view());
    }

    /// A touch long-press on an axis strip (`on_axis_long_press`).
    pub fn axis_long_press(&self, event: AxisLongPressEvent) {
        self.with(|st| st.emit(Event::AxisLongPress(event)));
    }

    /// Glide back to the most recent bars at the current zoom.
    pub fn scroll_to_realtime(&self) {
        self.with(|st| st.scroll_to_realtime());
    }

    /// Track the pointer's proximity to the scroll button (plot-relative px).
    pub fn scroll_proximity_move(&self, x: f64, y: f64) {
        self.with(|st| {
            let (w, h) = st.css_size;
            st.scroll_proximity_move(x, y, w, h);
        });
    }

    pub fn scroll_proximity_leave(&self) {
        self.with(|st| st.scroll_proximity_leave());
    }

    /// Pan by a whole number of bars (keyboard): positive toward the latest.
    pub fn pan_by_bars(&self, bars: f64) {
        self.with(|st| st.pan_by_bars(bars));
    }

    /// Zoom one keyboard notch, right-edge anchored (+1 in, -1 out).
    pub fn zoom_by_step(&self, direction: i32) {
        self.with(|st| st.zoom_by_step(direction));
    }

    /// Move the focused bar by `delta` from the crosshair bar (or the last bar).
    pub fn step_crosshair(&self, delta: i64) {
        self.with(|st| st.step_crosshair(delta));
    }

    /// Scroll the bar into view, draw the crosshair on it and announce it.
    pub fn focus_bar(&self, index: i64) {
        self.with(|st| st.focus_bar(index));
    }

    /// Run a classified keyboard intent (the original's `KeyboardController` dispatch).
    pub fn handle_key_action(&self, action: KeyAction) {
        self.with(|st| st.handle_key_action(action));
    }

    /// A keydown: the chart's own navigation keys. Returns whether the key mapped to
    /// an action (and so was handled).
    pub fn handle_key(&self, key: &KeyInput<'_>) -> bool {
        match key_to_action(key) {
            Some(action) => {
                self.handle_key_action(action);
                true
            }
            None => false,
        }
    }

    // ── public extras: queries for the host's input and chrome ──

    /// A snapshot of the coordinate system (`InputController.getCoords`).
    pub fn coordinates(&self) -> CoordinateSystem {
        self.read(|st| st.coords.clone())
    }

    /// The controller's feature switches (`zoomAnchor`, `axisDrag`, `paneResize`).
    pub fn input_settings(&self) -> InputSettings {
        self.read(|st| InputSettings {
            right_edge_zoom: st.zoom_anchor_right,
            axis_drag: st.axis_drag_enabled,
            pane_resize: st.pane_resize_enabled,
        })
    }

    /// The free-pan trigger, or `None` while free panning is off.
    pub fn free_pan_trigger(&self) -> Option<FreePanTrigger> {
        self.read(|st| st.free_pan_on.then(|| st.free_pan_trigger.clone()))
    }

    /// The user drawings take part in pointer input (`drawingsInteractive`).
    pub fn drawings_interactive(&self) -> bool {
        self.read(|st| st.drawings_interactive)
    }

    /// The pane's mark-lane glyph cursor hint: a lane glyph under the pointer reads
    /// as clickable (`drawingsCursor`'s fallback).
    pub fn mark_glyph_at(&self, x: f64, y: f64) -> bool {
        self.read(|st| st.chrome.mark_glyph_at(x, y).is_some())
    }

    /// Hover text of the mark glyph under a plot point, or `None`.
    pub fn mark_tooltip_at(&self, x: f64, y: f64) -> Option<String> {
        self.read(|st| {
            let groups = st.mark_groups_in_use();
            st.chrome.mark_tooltip_at(x, y, &groups, st.i18n.as_deref())
        })
    }

    /// The Pine label tooltip under a plot point (a label that carries one), else a
    /// mark tooltip.
    pub fn label_tooltip_at(&self, x: f64, y: f64) -> Option<String> {
        let label = self.read(|st| {
            st.indicator_slices
                .label_tooltip_at(x, y)
                .map(str::to_string)
        });
        label.or_else(|| self.mark_tooltip_at(x, y))
    }

    /// Reserve `px` of left gutter for the docked drawings toolbar (0 releases it)
    /// and re-lay-out the plot.
    pub fn set_toolbar_gutter(&self, px: f64) {
        self.with(|st| {
            if px == st.toolbar_gutter {
                return;
            }
            st.toolbar_gutter = px;
            st.publish_gutters();
            st.position_attribution();
            st.sync_size();
        });
    }

    /// The sticky magnet mode for user drawings.
    pub fn snap_mode(&self) -> SnapMode {
        self.read(|st| st.snap_mode)
    }

    pub fn set_snap_mode(&self, mode: SnapMode) {
        self.with(|st| st.snap_mode = mode);
    }

    /// Snap a data point `(time, price)` to the nearest candle for the drawing magnet
    /// (see the original's `snapToCandle`). `cursor_px` feeds the `Weak` distance test.
    pub fn snap_to_candle(
        &self,
        point: (f64, f64),
        pane_id: &str,
        mode: SnapMode,
        cursor_px: Option<(f64, f64)>,
    ) -> (f64, f64) {
        self.read(|st| st.snap_to_candle(point, pane_id, mode, cursor_px))
    }

    /// Run `f` with the data-to-pixel projector user drawings resolve their anchors
    /// through. `f` must not call back into the renderer.
    pub fn with_drawing_projector<R>(
        &self,
        f: impl FnOnce(&dyn crate::drawings::Projector) -> R,
    ) -> R {
        let st = self.st.borrow();
        let gateway = st.user_drawings.as_ref().and_then(|h| h.series_gateway());
        with_projector(&st.scene, &st.coords, gateway.as_deref(), f)
    }

    /// The look the price series actually paints with.
    pub fn series_look(&self) -> SeriesLook {
        self.read(|st| st.series_look())
    }

    /// The live plot theme the drawings paint with (the derived theme, not the chrome one).
    pub fn plot_theme(&self) -> VelaTheme {
        self.read(|st| st.theme.clone())
    }

    /// Repaint the user-drawings layer now, synchronously, without recomputing scales
    /// (the original's `userDrawings.render()`): hover, ghost, drag and selection changes
    /// call it. A no-op before the first data frame. Must not be called from inside a
    /// frame or a [`UserDrawingsHook`] method.
    pub fn repaint_user_drawings(&self) {
        self.with(|st| st.render_user_drawings());
    }

    /// The chart's bar interval, ms.
    pub fn chart_bar_ms(&self) -> f64 {
        self.read(|st| st.coords.bar_interval())
    }

    /// The display time zone.
    pub fn timezone(&self) -> String {
        self.read(|st| st.scene.timezone.clone())
    }

    /// The price axis decimals the symbol's tick size implies (`None` before known).
    pub fn price_decimals(&self) -> Option<f64> {
        self.read(|st| {
            st.scene
                .price_mintick
                .filter(|m| *m != 0.0)
                .map(crate::render::chrome::ticks::tick_decimals)
        })
    }

    /// The pane's series draw-order keys (see `SceneGraph::series_boundaries`).
    pub fn series_boundaries(&self, pane_id: &str) -> Vec<f64> {
        self.read(|st| st.scene.series_boundaries(pane_id))
    }

    /// The candles' draw-order key on the price pane (`None` elsewhere).
    pub fn price_z(&self, pane_id: &str) -> Option<f64> {
        self.read(|st| (pane_id == PRICE_PANE_ID).then_some(st.scene.candle_z))
    }

    /// A clone of a mounted indicator's model (its input schema and values feed the
    /// host's settings dialog).
    pub fn indicator_model(&self, id: &str) -> Option<IndicatorModel> {
        self.read(|st| st.scene.indicators.get(id).cloned())
    }

    /// The panes top to bottom, for the host's pane controls and axis buttons.
    pub fn pane_views(&self) -> Vec<PaneView> {
        self.read(|st| {
            let ordered = st.scene.ordered_panes();
            let count = ordered.len();
            ordered
                .iter()
                .enumerate()
                .map(|(i, p)| PaneView {
                    id: p.id.clone(),
                    kind: p.kind,
                    collapsed: p.collapsed,
                    maximized: st.maximized_pane_id.as_deref() == Some(p.id.as_str()),
                    top: p.bounds.top,
                    height: p.bounds.height,
                    index: i,
                    count,
                    auto: p.manual_scale.is_none(),
                    log: pane_log_scale(&st.scene, p),
                    invert: pane_invert(&st.scene, p),
                    mode: crate::render::core::scene_graph::pane_scale_mode(&st.scene, p),
                })
                .collect()
        })
    }

    /// A pane's pixel band plus the right-gutter width (what the legend pins to).
    pub fn pane_bounds_for(&self, pane_id: &str) -> (f64, f64, f64) {
        self.read(|st| {
            let p = st.scene.panes.get(pane_id);
            (
                p.map_or(0.0, |p| p.bounds.top),
                p.map_or(0.0, |p| p.bounds.height),
                st.right_axis_w,
            )
        })
    }

    /// The display title of a study pane's master (pane-scale) indicator.
    pub fn pane_master_title(&self, pane_id: &str) -> Option<String> {
        self.read(|st| {
            let models = st.scene.ordered_indicators_for_pane(pane_id);
            let master = models
                .iter()
                .find(|m| !m.own_scale)
                .or_else(|| models.first())?;
            let title = st.model_title(master);
            (!title.is_empty()).then_some(title)
        })
    }

    /// The right-gutter width (master column + merged scale columns), px.
    pub fn right_axis_width(&self) -> f64 {
        self.read(|st| st.right_axis_w)
    }

    /// The collapse toggle of the pane controls (a user action, reported to core).
    pub fn toggle_pane_collapse(&self, pane_id: &str) {
        self.with(|st| st.toggle_collapse(pane_id));
    }

    /// The maximize toggle of the pane controls (a user action, reported to core).
    pub fn toggle_pane_maximize(&self, pane_id: &str) {
        self.with(|st| st.toggle_maximize(pane_id));
    }

    /// Reorder a study pane one slot (a user action, reported to core).
    pub fn move_pane(&self, pane_id: &str, dir: PaneMoveDir) {
        self.with(|st| st.move_pane_local(pane_id, dir));
    }

    /// Toggle one pane's autoscale (the `A` axis button).
    pub fn toggle_pane_auto(&self, pane_id: &str) {
        self.with(|st| st.toggle_pane_auto(pane_id));
    }

    /// Toggle one pane's log scale (the `L` axis button).
    pub fn toggle_pane_log(&self, pane_id: &str) {
        self.with(|st| {
            let Some(p) = st.scene.panes.get(pane_id) else {
                return;
            };
            let on = !pane_log_scale(&st.scene, p);
            st.set_pane_log(pane_id, on);
        });
    }

    /// The price pane's layers back to front, as the legend restacks them: every
    /// indicator with a legend row, in paint order, plus the candles.
    pub fn overlay_stack(&self) -> Vec<StackEntry> {
        self.read(|st| st.overlay_stack())
    }

    /// What sits directly above / below a legend layer.
    pub fn layer_above(&self, id: &str) -> Option<Neighbor> {
        layer_above(&self.overlay_stack(), id)
    }

    pub fn layer_below(&self, id: &str) -> Option<Neighbor> {
        layer_below(&self.overlay_stack(), id)
    }

    /// Raise one legend layer a step in the price pane's stack.
    pub fn raise_overlay(&self, id: &str) {
        self.restack_overlay(id, raised_z);
    }

    /// Lower one legend layer a step in the price pane's stack.
    pub fn lower_overlay(&self, id: &str) {
        self.restack_overlay(id, lowered_z);
    }

    fn restack_overlay(&self, id: &str, step: fn(&[StackEntry], &str) -> Option<f64>) {
        self.with(|st| {
            // Shared keys are spread apart first (the order stays), so the step lands
            // exactly one layer over. No-op when there is nothing to step over.
            for fix in untie(&st.overlay_stack()) {
                st.scene.set_indicator_z(&fix.id, fix.z);
            }
            let Some(z) = step(&st.overlay_stack(), id) else {
                return;
            };
            st.scene.set_indicator_z(id, z);
            st.sync_legend_order();
            st.invalidate(InvalidateLevel::Full);
            // The stacking is part of the config: host chrome mirroring it follows.
            st.emit(Event::ConfigChanged);
        });
    }

    /// The legend's plot values as of the last frame's read bar, per indicator id
    /// (the same data pushed through `legend_plot_values`).
    pub fn legend_values(&self) -> Vec<(String, Vec<LegendPlotValue>)> {
        self.read(|st| {
            let n = st.bars.len() as i64;
            let mut out = Vec::new();
            if n == 0 {
                return out;
            }
            let idx = st.hover_logical.unwrap_or(n - 1);
            let price_pane = st.data_window_price_pane();
            for model in st.scene.indicators.values() {
                let rows = st.readout_rows_for(
                    model,
                    idx,
                    price_pane,
                    crate::model::SeriesSurface::Legend,
                );
                out.push((
                    model.id.clone(),
                    rows.into_iter()
                        .map(|r| LegendPlotValue {
                            value: r.value,
                            color: r.color,
                        })
                        .collect(),
                ));
            }
            out
        })
    }

    /// The legend title of a mounted indicator (the compact name).
    pub fn legend_title_of(&self, id: &str) -> Option<String> {
        self.read(|st| st.scene.indicators.get(id).map(|m| st.legend_title(m)))
    }

    /// The layout facts last published.
    pub fn layout_facts(&self) -> LayoutFacts {
        self.read(|st| st.layout)
    }

    /// The attribution mark's current state: the license requires the host to show it
    /// whenever `enabled`.
    pub fn attribution_state(&self) -> AttributionState {
        self.read(|st| AttributionState {
            enabled: st.attribution_enabled,
            html: st.attribution_html.clone(),
            left_px: st.toolbar_gutter + 12.0,
            bottom_px: st
                .attribution_sent
                .as_ref()
                .map_or(state::TIME_AXIS_H + 10.0, |a| a.bottom_px),
            ink: st.attribution_ink(),
        })
    }

    /// The document "Reset defaults" applies: every setting back to its first-run
    /// value, with two things that are NOT settings held or resolved here: the price
    /// style stays the one the user is looking at, and the timeline-mark groups are
    /// named back to their host-declared visibility. `None` before the first mount.
    pub fn factory_reset_document(&self) -> Option<Value> {
        self.read(|st| {
            let factory = st.factory_config.as_ref()?;
            let registry = st.chart_types.borrow();
            let prev = CandleColors {
                up_color: util::theme_string(&st.factory_theme, "upColor"),
                down_color: util::theme_string(&st.factory_theme, "downColor"),
            };
            let next = CandleColors {
                up_color: util::theme_string(&st.app_theme, "upColor"),
                down_color: util::theme_string(&st.app_theme, "downColor"),
            };
            // The snapshot's candle colors were its theme's; the reset gives the current theme's.
            let mut doc: ChartConfig = follow_theme_colors(
                &factory_reset_config(factory, Some(&st.chosen_style), &registry),
                &prev,
                &next,
            );
            for g in st.mark_groups_in_use() {
                doc.marks
                    .groups
                    .insert(g.id.clone(), g.visible != Some(false));
            }
            serde_json::to_value(doc).ok()
        })
    }

    /// Run the fresh synchronous paint a screenshot needs and list the canvases in
    /// compositing order (see [`ScreenshotPile`]). `None` before mount.
    pub fn screenshot_pile(&self) -> Option<ScreenshotPile> {
        self.with(|st| {
            st.canvases.as_ref()?;
            st.compute_scales();
            st.paint_data();
            let c = st.canvases.as_ref()?;
            let (below, above) = st.ordered_layer_canvases_for_pile();
            let mut images: Vec<CanvasImage> = vec![c.backdrop.image()];
            images.extend(below);
            images.extend([c.data.image(), c.volume.image(), c.vpvr.image()]);
            images.extend(above);
            images.extend([c.chrome.image(), c.drawings.image()]);
            Some(ScreenshotPile {
                background: util::theme_string(&st.theme, "background"),
                width: st.backing.0,
                height: st.backing.1,
                images,
            })
        })
    }

    /// The symbol picker the host's settings dialog opens for `input.symbol`.
    pub fn symbol_picker(&self) -> Option<SymbolPickerFn> {
        self.symbol_picker.borrow().clone()
    }

    pub fn legend_actions_provider(&self) -> Option<LegendActionProvider> {
        self.legend_actions.borrow().clone()
    }

    pub fn legend_callouts_provider(&self) -> Option<LegendCalloutProvider> {
        self.legend_callouts.borrow().clone()
    }

    pub fn legend_overview_action(&self) -> Option<Rc<dyn Fn()>> {
        self.legend_overview.borrow().clone()
    }

    /// The host-app settings tabs the dialog shows.
    pub fn settings_sections(&self) -> Vec<SettingsSection> {
        self.settings_sections.borrow().clone()
    }

    /// The setting ids hidden by the host's policy.
    pub fn hidden_settings(&self) -> Vec<String> {
        self.read(|st| st.hidden_settings.clone())
    }

    /// Every group the mark lane knows: the defined ones, then those marks name
    /// without a definition.
    pub fn mark_groups(&self) -> Vec<MarkGroup> {
        self.read(|st| st.mark_groups_in_use())
    }

    /// Whether the renderer is mounted (between `mount` and `destroy`).
    pub fn is_mounted(&self) -> bool {
        self.read(|st| st.mounted)
    }
}

impl State {
    /// Every group the lane knows: the defined ones, then those marks name without a
    /// definition.
    pub(crate) fn mark_groups_in_use(&self) -> Vec<MarkGroup> {
        let marks: Vec<Rc<TimelineMark>> = self
            .scene
            .timeline_marks
            .iter()
            .cloned()
            .map(Rc::new)
            .collect();
        effective_mark_groups(&marks, &self.scene.mark_groups)
    }

    /// The price pane's layers back to front, as the legend restacks them.
    pub(crate) fn overlay_stack(&self) -> Vec<StackEntry> {
        let painted: Vec<StackEntry> = self
            .scene
            .ordered_indicators_for_pane(PRICE_PANE_ID)
            .into_iter()
            .filter(|m| m.legend != Some(false))
            .map(|m| StackEntry::indicator(m.id.clone(), self.scene.z_of(&m.id)))
            .collect();
        price_stack(&painted, self.scene.candle_z)
    }
}

// ── the chart-renderer port ──

impl ChartRenderer for NativeRenderer {
    fn capabilities(&self) -> RendererCapabilities {
        NATIVE_CAPABILITIES
    }

    fn name(&self) -> &str {
        "native"
    }

    fn features(&self) -> Vec<String> {
        features::FEATURES
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    fn apply_feature(&self, key: &str, value: &Value) {
        if key == "priceStyle" {
            let style = value
                .as_str()
                .filter(|s| {
                    self.read(|st| {
                        price_style_ids(&st.chart_types.borrow())
                            .iter()
                            .any(|k| k == s)
                    })
                })
                .unwrap_or("candles")
                .to_string();
            self.show_price_style(Some(style));
            self.with(|st| st.invalidate(InvalidateLevel::Full));
            return;
        }
        self.with(|st| st.apply_feature(key, value));
    }

    fn read_feature(&self, key: &str) -> Option<Value> {
        self.read(|st| st.read_feature(key))
    }

    fn mount(&self, theme: &VelaTheme) {
        self.with(|st| st.mount(theme));
    }

    fn set_theme(&self, theme: &VelaTheme) {
        self.with(|st| st.set_theme(theme));
    }

    fn set_localization(&self, i18n: Rc<dyn Translator>, root: RendererRootAttributes) {
        self.with(|st| st.set_localization(i18n, root));
    }

    fn resize(&self) {
        self.with(|st| st.sync_size());
    }

    fn destroy(&self) {
        self.with(|st| st.destroy());
    }

    fn set_bars(&self, bars: &[Ohlcv], opts: SetBarsOptions) {
        self.with(|st| st.set_bars(bars, opts.preserve_view));
    }

    fn update_bar(&self, bar: &Ohlcv) {
        self.with(|st| st.update_bar(bar));
    }

    fn set_price_precision(&self, mintick: Option<f64>) {
        self.with(|st| st.set_price_precision(mintick));
    }

    fn supports_set_price_precision(&self) -> bool {
        true
    }

    /// The active symbol's style-deciding facts. A chosen style they rule out draws as
    /// its fallback.
    fn set_symbol_facts(&self, facts: Option<SymbolFacts>) {
        let before = self.read(|st| st.scene.price_style.clone());
        self.with(|st| st.symbol_facts = facts);
        self.show_price_style(None);
        self.with(|st| {
            if st.scene.price_style != before {
                st.invalidate(InvalidateLevel::Full);
            }
        });
    }

    fn set_native_data(&self, layer_type: &str, data: &Value) {
        self.with(|st| st.set_native_data(layer_type, data));
    }

    fn supports_set_native_data(&self) -> bool {
        true
    }

    fn set_timeline_marks(&self, marks: &[TimelineMark], groups: &[MarkGroup]) {
        self.with(|st| st.set_timeline_marks(marks, groups));
    }

    fn supports_set_timeline_marks(&self) -> bool {
        true
    }

    fn on_mark_click(&self, cb: Listener<MarkClickEvent>) -> Option<Unsubscribe> {
        Some(self.mark_click_cbs.add(cb))
    }

    fn set_indicator_status(&self, handle: &IndicatorRenderHandle, status: IndicatorStatus) {
        self.with(|st| st.set_indicator_status(&handle.id, status));
    }

    fn set_loading(&self, loading: bool) {
        self.with(|st| st.set_loading(loading));
    }

    fn ensure_pane(&self, pane: &Pane) {
        self.with(|st| st.ensure_pane(pane));
    }

    fn remove_pane(&self, id: &str) {
        self.with(|st| st.remove_pane(id));
    }

    fn set_indicator_pane(&self, handle: &IndicatorRenderHandle, pane_id: &str, own_scale: bool) {
        self.with(|st| st.set_indicator_pane(&handle.id, pane_id, own_scale));
    }

    fn supports_set_indicator_pane(&self) -> bool {
        true
    }

    fn order_panes(&self, ordered_ids: &[String]) {
        self.with(|st| st.order_panes(ordered_ids));
    }

    fn set_pane_collapsed(&self, pane_id: &str, collapsed: bool) {
        self.with(|st| st.set_pane_collapsed(pane_id, collapsed));
    }

    fn set_pane_maximized(&self, pane_id: Option<&str>) {
        self.with(|st| st.set_pane_maximized(pane_id));
    }

    fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
        self.with(|st| st.set_pane_height_weight(pane_id, weight));
    }

    fn supports_set_pane_height_weight(&self) -> bool {
        true
    }

    fn pane_height_weight(&self, pane_id: &str) -> Option<f64> {
        self.read(|st| st.scene.panes.get(pane_id).map(|p| p.height_weight))
    }

    fn on_pane_action(&self, cb: Listener<PaneAction>) -> Option<Unsubscribe> {
        Some(self.pane_action_cbs.add(cb))
    }

    fn on_move_indicator(&self, cb: MoveIndicatorListener) -> Option<Unsubscribe> {
        Some(self.move_indicator_cbs.add(cb))
    }

    fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
        self.with(|st| st.mount_indicator(model));
        IndicatorRenderHandle {
            id: model.id.clone(),
        }
    }

    fn update_indicator(&self, handle: &IndicatorRenderHandle, patch: &ScenePatch) {
        self.with(|st| st.update_indicator(&handle.id, patch));
    }

    fn remove_indicator(&self, handle: &IndicatorRenderHandle) {
        self.with(|st| st.remove_indicator(&handle.id));
    }

    fn set_indicator_inputs(
        &self,
        handle: &IndicatorRenderHandle,
        values: &crate::model::Inputs,
        props: Option<&crate::model::Inputs>,
    ) {
        self.host.legend_set_values(&handle.id, values, props);
    }

    fn set_symbol_picker(&self, picker: Option<SymbolPickerFn>) {
        let set = picker.is_some();
        *self.symbol_picker.borrow_mut() = picker;
        self.with(|st| st.symbol_picker_set = set);
        self.host.legend_providers_changed();
    }

    fn supports_set_symbol_picker(&self) -> bool {
        true
    }

    fn set_legend_actions(&self, provider: Option<LegendActionProvider>) {
        *self.legend_actions.borrow_mut() = provider;
        self.host.legend_providers_changed();
    }

    fn set_legend_callouts(&self, provider: Option<LegendCalloutProvider>) {
        *self.legend_callouts.borrow_mut() = provider;
        self.host.legend_providers_changed();
    }

    fn set_legend_overview_action(&self, action: Option<Rc<dyn Fn()>>) {
        *self.legend_overview.borrow_mut() = action;
        self.host.legend_providers_changed();
    }

    fn open_indicator_settings(&self, indicator_id: &str) {
        self.host.open_indicator_settings(indicator_id);
    }

    fn supports_open_indicator_settings(&self) -> bool {
        self.host.has_dialogs()
    }

    fn set_indicator_visible(&self, handle: &IndicatorRenderHandle, visible: bool) {
        self.with(|st| st.set_indicator_visible(&handle.id, visible));
    }

    fn on_price_style_change(&self, cb: Listener<PriceStyle>) -> Option<Unsubscribe> {
        Some(self.price_style_cbs.add(cb))
    }

    fn on_price_style_will_change(&self, cb: PriceStyleWillChangeListener) -> Option<Unsubscribe> {
        Some(self.price_style_will_cbs.add(cb))
    }

    fn on_input_change(&self, cb: Listener<InputChangeEvent>) -> Unsubscribe {
        self.input_change_cbs.add(cb)
    }

    fn on_remove_indicator(&self, cb: Listener<String>) -> Unsubscribe {
        self.remove_indicator_cbs.add(cb)
    }

    fn on_toggle_indicator_visible(&self, cb: ToggleVisibleListener) -> Option<Unsubscribe> {
        Some(self.toggle_visible_cbs.add(cb))
    }

    fn on_crosshair_move(&self, cb: Listener<CrosshairEvent>) -> Unsubscribe {
        self.crosshair_cbs.add(cb)
    }

    fn on_click(&self, cb: Listener<ClickEvent>) -> Unsubscribe {
        self.click_cbs.add(cb)
    }

    fn on_axis_long_press(&self, cb: Listener<AxisLongPressEvent>) -> Option<Unsubscribe> {
        Some(self.axis_long_press_cbs.add(cb))
    }

    fn set_external_crosshair(&self, time: Option<Millis>, price: Option<f64>) {
        self.with(|st| st.set_external_crosshair(time, price));
    }

    fn supports_set_external_crosshair(&self) -> bool {
        true
    }

    fn get_data_window_readout(&self) -> Option<DataWindowReadout> {
        Some(self.read(|st| st.data_window_readout()))
    }

    fn get_visible_range(&self) -> Option<VisibleRange> {
        self.read(|st| st.get_visible_range())
    }

    fn set_visible_range(&self, range: VisibleRange, opts: VisibleRangeOptions) {
        self.with(|st| st.set_visible_range(range, opts.right_margin));
    }

    fn pan_by(&self, fraction: f64) {
        self.with(|st| st.pan_by(fraction));
    }

    fn supports_pan_by(&self) -> bool {
        true
    }

    fn on_viewport_change(&self, cb: Listener<VisibleRange>) -> Unsubscribe {
        self.viewport_cbs.add(cb)
    }

    fn get_config(&self) -> Option<Value> {
        self.read(|st| serde_json::to_value(st.get_config()).ok())
    }

    /// Apply a (possibly partial) config document. Untrusted JSON is validated and
    /// merged onto the current config, so malformed fields are dropped and a partial
    /// patch only changes what it names. Repaints with NO indicator re-run.
    fn apply_config(&self, config: &Value) {
        let next = self.with(|st| st.apply_config_head(config));
        // The price style is written between the two halves, announcing the switch.
        self.show_price_style(Some(next.series.style.clone()));
        self.with(|st| {
            st.apply_config_tail(&next);
            // The settings dialog commits through apply_config: this is how host chrome
            // that mirrors a config value (the bottom-bar timezone) learns about
            // in-chart edits.
            st.emit(Event::ConfigChanged);
        });
    }

    fn supports_apply_config(&self) -> bool {
        true
    }

    fn on_config_changed(&self, cb: Box<dyn FnMut()>) -> Option<Unsubscribe> {
        Some(self.config_changed_cbs.add(cb))
    }

    fn on_theme_select(&self, cb: Listener<ThemeId>) -> Option<Unsubscribe> {
        Some(self.theme_select_cbs.add(cb))
    }

    fn focus(&self) {
        self.host.focus();
    }

    fn close_dialogs(&self) {
        self.host.close_dialogs();
    }

    fn open_settings_dialog(&self, section: Option<&str>) {
        self.host.open_settings_dialog(section);
    }

    fn on_chart_type_settings_change(
        &self,
        cb: crate::ports::chart_renderer::ChartTypeSettingsListener,
    ) -> Option<Unsubscribe> {
        Some(self.chart_type_settings_cbs.add(cb))
    }

    fn set_settings_sections(&self, sections: &[SettingsSection]) {
        *self.settings_sections.borrow_mut() = sections.to_vec();
        self.host.settings_changed();
    }

    fn set_settings_visibility(&self, policy: &SettingsVisibility) {
        self.with(|st| st.hidden_settings = policy.hidden.clone().unwrap_or_default());
        self.host.settings_changed();
    }

    fn list_settings_ids(&self) -> Vec<String> {
        let sections: Vec<HostSectionIdSource> = self
            .settings_sections
            .borrow()
            .iter()
            .map(|s| HostSectionIdSource {
                title: s.title.clone(),
                id: s.id.clone(),
                rows: s
                    .rows
                    .iter()
                    .map(|r| HostRowIdSource {
                        kind: r
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        label: r
                            .get("label")
                            .and_then(Value::as_str)
                            .map(|l| crate::i18n::MessageLike::from(l.to_string()))
                            .unwrap_or_default(),
                        id: r.get("id").and_then(Value::as_str).map(str::to_string),
                    })
                    .collect(),
            })
            .collect();
        self.read(|st| {
            let groups = st.mark_groups_in_use();
            let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
            settings_id_catalog(&st.chart_types.borrow(), &sections, &ids)
        })
    }

    fn set_layout_mode(&self, mode: LayoutMode) {
        self.with(|st| st.set_layout_mode(mode));
    }

    fn set_wall_clock(&self, clock: Option<Rc<dyn WallClock>>) {
        self.with(|st| st.set_wall_clock(clock));
    }

    fn user_drawings_port(&self) -> Option<Rc<dyn DrawingsRendererPort>> {
        self.user_port.borrow().clone()
    }
}
