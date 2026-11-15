//! The shell's mutable state: the original `NativeRenderer`'s private fields.
//!
//! Everything lives in one struct behind a single `RefCell` (see
//! [`NativeRenderer`](super::NativeRenderer)). Logic that needs it is written as
//! `impl State` blocks in the sibling files; the public wrapper locks it, runs one
//! operation and then delivers what the operation queued ([`Event`]s for
//! subscribers, a synchronous flush request), so no subscriber callback ever runs
//! with the state borrowed.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use serde_json::{Map, Value};

use crate::chart_types::ChartTypeRegistry;
use crate::i18n::Translator;
use crate::marks::MarkClickEvent;
use crate::model::Ohlcv;
use crate::options::{
    AUTOSCALE_EASE_DEFAULT_MS, FreePanTrigger, INTRO_DURATION_DEFAULT_MS, IntroAnimation,
    IntroStyle, LIVE_BAR_EASE_DEFAULT_MS, NativeBackend, PAN_INERTIA_DEFAULT_MS,
    RendererDisplayOptions, SCROLL_EASE_DEFAULT_MS, ZOOM_EASE_DEFAULT_MS,
};
use crate::palette::{BEARISH, BULLISH};
use crate::ports::chart_renderer::{
    AxisLongPressEvent, ClickEvent, CrosshairEvent, PaneAction, RendererRootAttributes, VelaTheme,
    VisibleRange, WallClock,
};
use crate::ports::market_data::SymbolFacts;
use crate::render::backdrop::BackdropRenderer;
use crate::render::backend::canvas2d_backend::Canvas2dBackend;
use crate::render::backend::layers::{
    LayerRegistry, RendererLayerDefinition, RendererLayerInstance,
};
use crate::render::backend::render_backend::{BackendKind, RenderBackend};
use crate::render::backend::webgl2_backend::WebGL2Backend;
use crate::render::chrome::chrome_renderer::ChromeRenderer;
use crate::render::chrome::crosshair_renderer::CrosshairRenderer;
use crate::render::chrome::price_axis_ticks::PriceAxisTickSource;
use crate::render::core::chart_config::{ChartConfig, base_painting_of, candle_override_for};
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::core::{
    Animator, CoordinateSystem, EaseSetting, PriceScale, SceneGraph, Scheduler, ViewportState,
    default_viewport,
};
use crate::render::drawings::indicator_slices::IndicatorDrawingSlices;
use crate::render::shared::drawing_scene_renderer::DrawingSceneRenderer;
use crate::render::volume::VolumeRenderer;
use crate::render::vpvr::VpvrRenderer;
use crate::runtime::Unsubscribe;
use crate::theme::dark_theme;

use super::NativeRenderer;
use super::host::{HostCanvas, HostGlCanvas, NativeHost};
use super::user_drawings::UserDrawingsHook;

pub(crate) const PRICE_PANE_ID: &str = "price";
/// The 'weak' magnet only snaps when a candle point is within this many px of the cursor.
pub(crate) const WEAK_SNAP_PX: f64 = 8.0;
pub(crate) const COLLAPSED_PANE_H: f64 = 26.0;
pub(crate) const TIME_AXIS_H: f64 = 22.0;
pub(crate) const SCROLL_BTN_RIGHT_INSET: f64 = 12.0;
/// Initial scroll-button bottom offset: clears the time axis plus a gap.
pub(crate) const SCROLL_BTN_BOTTOM: f64 = TIME_AXIS_H + 14.0;
pub(crate) const SCROLL_BTN_PROXIMITY_PX: f64 = 120.0;
/// Size of the scroll-to-realtime button the proximity test centers on (desktop).
pub(crate) const SCROLL_BTN_SIZE: f64 = 26.0;
/// Never zoom or pan so far that fewer than this many candles stay on screen.
pub(crate) const MIN_VISIBLE_BARS: f64 = 2.0;
/// Breathing room at max zoom-out: all bars plus this margin fill the width.
pub(crate) const ZOOM_OUT_MARGIN_BARS: f64 = 6.0;
/// After the candle reveal the indicator models fade in over this long.
pub(crate) const INTRO_MODEL_FADE_MS: f64 = 350.0;
/// Fling ends when on-screen motion drops below this (px/ms).
pub(crate) const FLING_STOP_PX: f64 = 0.02;
/// A content-less mark click's filled flash.
pub(crate) const MARK_FLASH_MS: f64 = 220.0;
/// Price-axis drag sensitivity: the visible price span scales by e^(dy*k).
pub(crate) const PRICE_SCALE_K: f64 = 0.004;
/// Keyboard zoom step: bar spacing scales by e^(+-k) per press.
pub(crate) const KEY_ZOOM_STEP: f64 = 0.2;
/// Pixel half-thickness of a pane separator's hit zone.
pub(crate) const SEPARATOR_HIT_PX: f64 = 4.0;

/// Something to tell the subscribers once the state is released.
pub(crate) enum Event {
    Viewport(VisibleRange),
    Crosshair(CrosshairEvent),
    Click(ClickEvent),
    AxisLongPress(AxisLongPressEvent),
    PaneAction(PaneAction),
    MarkClick(MarkClickEvent),
    ConfigChanged,
    ChartTypeSettings(String, Map<String, Value>),
    PriceStyle(String),
}

/// One mounted SDK layer: a registered definition's base entry, or the dedicated
/// entry of one native indicator instance.
pub(crate) struct ExtLayer {
    pub def: RendererLayerDefinition,
    pub instance: Box<dyn RendererLayerInstance>,
    pub canvas: Rc<dyn HostCanvas>,
    /// The native-data channel this entry paints (`def.id` for the base entry).
    pub channel: String,
    /// The indicator instance this entry is dedicated to (`None` = the base entry).
    pub owner: Option<String>,
}

/// The data canvas: a 2D canvas, or the host's WebGL2 canvas when the geometry backend
/// is WebGL2.
pub(crate) enum DataCanvas {
    Canvas2d(Rc<dyn HostCanvas>),
    Gl(Rc<dyn HostGlCanvas>),
}

impl DataCanvas {
    pub fn image(&self) -> crate::render::canvas::CanvasImage {
        match self {
            DataCanvas::Canvas2d(c) => c.image(),
            DataCanvas::Gl(c) => c.image(),
        }
    }

    pub fn set_size(&self, bw: f64, bh: f64, cw: f64, ch: f64) {
        match self {
            DataCanvas::Canvas2d(c) => c.set_size(bw, bh, cw, ch),
            DataCanvas::Gl(c) => c.set_size(bw, bh, cw, ch),
        }
    }
}

/// The L0 geometry backend in force: chosen at mount (WebGL2 if allowed and offered by
/// the host, else canvas2d) and swapped to canvas2d if the GL context becomes unusable.
pub(crate) enum GeometryBackend {
    Canvas2d(Canvas2dBackend),
    WebGl2(Box<WebGL2Backend>),
}

impl GeometryBackend {
    pub fn set_backing_size(&mut self, bw: f64, bh: f64) {
        match self {
            GeometryBackend::Canvas2d(b) => b.set_backing_size(bw, bh),
            GeometryBackend::WebGl2(b) => b.set_backing_size(bw, bh),
        }
    }

    /// The `glow` feature: only the WebGL2 backend has a bloom pass.
    pub fn set_glow(&mut self, glow: f64) {
        if let GeometryBackend::WebGl2(b) = self {
            b.glow = glow;
        }
    }

    fn inner(&self) -> &dyn RenderBackend {
        match self {
            GeometryBackend::Canvas2d(b) => b,
            GeometryBackend::WebGl2(b) => b.as_ref(),
        }
    }

    fn inner_mut(&mut self) -> &mut dyn RenderBackend {
        match self {
            GeometryBackend::Canvas2d(b) => b,
            GeometryBackend::WebGl2(b) => b.as_mut(),
        }
    }
}

impl RenderBackend for GeometryBackend {
    fn kind(&self) -> BackendKind {
        self.inner().kind()
    }
    fn model_alpha(&self) -> f64 {
        self.inner().model_alpha()
    }
    fn set_model_alpha(&mut self, alpha: f64) {
        self.inner_mut().set_model_alpha(alpha);
    }
    fn candle_body_alpha(&self) -> f64 {
        self.inner().candle_body_alpha()
    }
    fn set_candle_body_alpha(&mut self, alpha: f64) {
        self.inner_mut().set_candle_body_alpha(alpha);
    }
    fn candle_structure_alpha(&self) -> f64 {
        self.inner().candle_structure_alpha()
    }
    fn set_candle_structure_alpha(&mut self, alpha: f64) {
        self.inner_mut().set_candle_structure_alpha(alpha);
    }
    fn candle_body_scale(&self) -> f64 {
        self.inner().candle_body_scale()
    }
    fn set_candle_body_scale(&mut self, scale: f64) {
        self.inner_mut().set_candle_body_scale(scale);
    }
    fn render(&mut self, scene: &SceneGraph, coords: &CoordinateSystem, theme: &VelaTheme) {
        self.inner_mut().render(scene, coords, theme);
    }
    fn destroy(&mut self) {
        self.inner_mut().destroy();
    }
}

pub(crate) struct Canvases {
    pub backdrop: Rc<dyn HostCanvas>,
    pub data: DataCanvas,
    pub volume: Rc<dyn HostCanvas>,
    pub vpvr: Rc<dyn HostCanvas>,
    pub chrome: Rc<dyn HostCanvas>,
    pub drawings: Rc<dyn HostCanvas>,
    pub cursor: Rc<dyn HostCanvas>,
}

/// A vertically scalable window: a pane's master scale or a merged indicator's own
/// scale column. Both expose the same four fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScaleHolder {
    Pane(String),
    Indicator(String),
}

/// Which way the marks fan was opened: a hover folds when the pointer leaves, a
/// tap only on a tap elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpandedBy {
    Hover,
    Tap,
}

pub(crate) struct State {
    pub me: Weak<NativeRenderer>,
    pub host: Rc<dyn NativeHost>,
    pub sched: Rc<Scheduler>,
    pub anim: Rc<Animator>,
    /// Queued for the subscribers (see the module doc).
    pub outbox: Vec<Event>,
    /// A synchronous repaint to run after the state is released.
    pub flush: Option<InvalidateLevel>,
    pub chart_types: Rc<RefCell<ChartTypeRegistry>>,
    pub layers: LayerRegistry,

    pub mounted: bool,
    pub theme: VelaTheme,
    /// The app theme as given, before the config's candle colors.
    pub app_theme: VelaTheme,
    pub surface_background: String,
    pub surface_text_color: String,
    pub i18n: Option<Rc<dyn Translator>>,
    pub root_attrs: RendererRootAttributes,
    pub toolbar_gutter: f64,
    pub canvases: Option<Canvases>,
    pub ext_layers: Vec<ExtLayer>,
    pub layer_order_sig: String,
    pub attribution_enabled: bool,
    pub attribution_html: Option<String>,

    pub coords: CoordinateSystem,
    pub scene: SceneGraph,
    pub backend: GeometryBackend,
    pub backend_mode: NativeBackend,
    pub glow_amount: f64,
    pub price_axis_ticks: Rc<PriceAxisTickSource>,
    pub backdrop: BackdropRenderer,
    pub volume: VolumeRenderer,
    pub vpvr: VpvrRenderer,
    pub chrome: ChromeRenderer,
    pub crosshair_layer: CrosshairRenderer,
    pub indicator_slices: IndicatorDrawingSlices,
    /// The shared script-drawing renderer behind the autoscale's drawing range.
    pub draw_scene: DrawingSceneRenderer,

    pub host_clock: Option<Rc<dyn WallClock>>,
    pub countdown_unsub: Option<Unsubscribe>,
    pub symbol_picker_set: bool,
    pub indicator_titles_on: bool,
    pub indicator_values_on: bool,
    pub keyboard_enabled: bool,
    pub history_chords_enabled: bool,

    pub anim_zoom: EaseSetting,
    pub anim_pan: EaseSetting,
    pub anim_scroll: EaseSetting,
    pub anim_autoscale: EaseSetting,
    pub anim_live_bar: EaseSetting,
    pub candle_up: String,
    pub candle_down: String,
    pub intro: IntroAnimation,
    pub intro_on_style: IntroStyle,
    pub intro_played: bool,
    pub intro_raf: Option<u32>,
    pub loading: bool,
    /// Indicator-model opacity: 0 during the candle reveal, fades to 1 after.
    pub model_alpha: f64,
    pub target_bar_spacing: f64,
    pub zoom_anchor_logical: f64,
    pub zoom_anchor_x: f64,
    /// Wheel-zoom anchors the right edge (`true`, the default) or the cursor.
    pub zoom_anchor_right: bool,
    pub pan_velocity: f64,

    pub bars: Vec<Ohlcv>,
    pub did_initial_fit: bool,
    pub painted_data: bool,
    pub skeleton_clock_ms: f64,
    pub candle_body_alpha: f64,
    pub candle_structure_alpha: f64,
    pub last_pointer: Option<(f64, f64)>,

    pub volume_active: bool,
    pub volume_hidden: bool,
    pub vpvr_active: bool,
    pub vpvr_hidden: bool,

    pub live_ease_time: i64,
    pub live_ease_high: f64,
    pub live_ease_low: f64,
    pub live_ease_close: f64,

    pub axis_drag_enabled: bool,
    pub free_pan_on: bool,
    pub free_pan_trigger: FreePanTrigger,
    pub scale_drag_holder: Option<ScaleHolder>,
    pub scale_drag_height: f64,
    pub scale_drag_start: Option<PriceScale>,

    pub pane_resize_enabled: bool,
    pub resize_above: Option<String>,
    pub resize_below: Option<String>,
    pub resize_split_start: Option<crate::render::core::pane_resize::PaneSplit>,
    pub resize_start_above_weight: f64,
    pub drawings_interactive: bool,
    pub hover_separator_y: Option<f64>,

    pub maximized_pane_id: Option<String>,
    pub right_axis_w: f64,
    pub hover_logical: Option<i64>,

    pub settings_enabled: bool,
    pub dialog_host_set: bool,
    pub hidden_settings: Vec<String>,

    pub pointer_near_scroll_btn: bool,
    pub scroll_target_ro: Option<f64>,
    pub scroll_btn_bottom_px: f64,
    pub scroll_btn_right_px: f64,
    pub layout_mode: crate::ports::chart_renderer::LayoutMode,
    pub layout: super::host::LayoutFacts,

    pub factory_config: Option<ChartConfig>,
    pub factory_theme: VelaTheme,
    pub chosen_style: String,
    pub symbol_facts: Option<SymbolFacts>,
    pub snap_mode: crate::drawings::SnapMode,
    pub external_cross: Option<(i64, Option<f64>)>,

    pub marks_expanded_by: ExpandedBy,
    pub mark_pulse_raf: Option<u32>,
    pub mark_popover_key: Option<String>,

    pub user_drawings: Option<Rc<dyn UserDrawingsHook>>,
    /// Backing size of the pile in device px, and its CSS size.
    pub backing: (f64, f64),
    pub css_size: (f64, f64),
    /// What the host was last told, so it is told again only on change.
    pub shade_sent: Option<(f64, f64, f64)>,
    pub scroll_btn_sent: Option<super::host::ScrollButtonState>,
    pub layout_sent: Option<super::host::LayoutFacts>,
    pub attribution_sent: Option<super::host::AttributionState>,
    pub legend_order_sent: Option<Vec<String>>,
}

pub(crate) fn derive_theme(theme: &VelaTheme, up: &str, down: &str) -> VelaTheme {
    let mut out = theme.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("upColor".into(), Value::String(up.to_string()));
        obj.insert("downColor".into(), Value::String(down.to_string()));
    }
    out
}

impl State {
    pub(crate) fn new(
        me: Weak<NativeRenderer>,
        host: Rc<dyn NativeHost>,
        sched: Rc<Scheduler>,
        anim: Rc<Animator>,
        chart_types: Rc<RefCell<ChartTypeRegistry>>,
        layers: LayerRegistry,
        opts: Option<RendererDisplayOptions>,
    ) -> Self {
        let price_axis_ticks = Rc::new(PriceAxisTickSource::new());
        let dark = dark_theme();
        let surface_background = dark
            .get("background")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let surface_text_color = dark
            .get("textColor")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut st = State {
            me,
            host,
            sched,
            anim,
            outbox: Vec::new(),
            flush: None,
            chart_types,
            layers,
            mounted: false,
            theme: dark.clone(),
            app_theme: dark.clone(),
            surface_background,
            surface_text_color,
            i18n: None,
            root_attrs: RendererRootAttributes::default(),
            toolbar_gutter: 0.0,
            canvases: None,
            ext_layers: Vec::new(),
            layer_order_sig: String::new(),
            attribution_enabled: true,
            attribution_html: None,
            coords: CoordinateSystem::new(),
            scene: SceneGraph::new(),
            backend: GeometryBackend::Canvas2d(Canvas2dBackend::new()),
            backend_mode: NativeBackend::Auto,
            glow_amount: 0.0,
            price_axis_ticks: price_axis_ticks.clone(),
            backdrop: BackdropRenderer::new(price_axis_ticks.clone()),
            volume: VolumeRenderer::new(),
            vpvr: VpvrRenderer::new(),
            chrome: ChromeRenderer::new(price_axis_ticks),
            crosshair_layer: CrosshairRenderer::new(),
            indicator_slices: IndicatorDrawingSlices::new(),
            draw_scene: DrawingSceneRenderer::new(),
            host_clock: None,
            countdown_unsub: None,
            symbol_picker_set: false,
            indicator_titles_on: true,
            indicator_values_on: true,
            keyboard_enabled: true,
            history_chords_enabled: true,
            anim_zoom: EaseSetting::new(ZOOM_EASE_DEFAULT_MS),
            anim_pan: EaseSetting::new(PAN_INERTIA_DEFAULT_MS),
            anim_scroll: EaseSetting::new(SCROLL_EASE_DEFAULT_MS),
            anim_autoscale: EaseSetting::new(AUTOSCALE_EASE_DEFAULT_MS),
            // The forming-bar glide ships off.
            anim_live_bar: EaseSetting::with_initial(LIVE_BAR_EASE_DEFAULT_MS, 0.0),
            candle_up: BULLISH.to_string(),
            candle_down: BEARISH.to_string(),
            intro: IntroAnimation {
                style: Some(IntroStyle::Settle),
                duration: INTRO_DURATION_DEFAULT_MS,
            },
            intro_on_style: IntroStyle::Settle,
            intro_played: false,
            intro_raf: None,
            loading: false,
            model_alpha: 1.0,
            target_bar_spacing: 0.0,
            zoom_anchor_logical: 0.0,
            zoom_anchor_x: 0.0,
            zoom_anchor_right: true,
            pan_velocity: 0.0,
            bars: Vec::new(),
            did_initial_fit: false,
            painted_data: false,
            skeleton_clock_ms: 0.0,
            candle_body_alpha: 1.0,
            candle_structure_alpha: 1.0,
            last_pointer: None,
            volume_active: false,
            volume_hidden: false,
            vpvr_active: false,
            vpvr_hidden: false,
            live_ease_time: 0,
            live_ease_high: 0.0,
            live_ease_low: 0.0,
            live_ease_close: 0.0,
            axis_drag_enabled: true,
            free_pan_on: false,
            free_pan_trigger: FreePanTrigger::Drag,
            scale_drag_holder: None,
            scale_drag_height: 0.0,
            scale_drag_start: None,
            pane_resize_enabled: true,
            resize_above: None,
            resize_below: None,
            resize_split_start: None,
            resize_start_above_weight: 0.0,
            drawings_interactive: true,
            hover_separator_y: None,
            maximized_pane_id: None,
            right_axis_w: crate::render::chrome::axis_layout::AXIS_MASTER_W,
            hover_logical: None,
            settings_enabled: false,
            dialog_host_set: false,
            hidden_settings: Vec::new(),
            pointer_near_scroll_btn: false,
            scroll_target_ro: None,
            scroll_btn_bottom_px: SCROLL_BTN_BOTTOM,
            scroll_btn_right_px: crate::render::chrome::axis_layout::AXIS_MASTER_W
                + SCROLL_BTN_RIGHT_INSET,
            layout_mode: Default::default(),
            layout: Default::default(),
            factory_config: None,
            factory_theme: dark,
            chosen_style: "candles".to_string(),
            symbol_facts: None,
            snap_mode: Default::default(),
            external_cross: None,
            marks_expanded_by: ExpandedBy::Hover,
            mark_pulse_raf: None,
            mark_popover_key: None,
            user_drawings: None,
            backing: (0.0, 0.0),
            css_size: (0.0, 0.0),
            shade_sent: None,
            scroll_btn_sent: None,
            layout_sent: None,
            attribution_sent: None,
            legend_order_sent: None,
        };
        st.layout.scale_gutter = st.right_axis_w;
        if let Some(opts) = opts {
            st.scene.show_price_line = opts.current_price_line;
            st.scene.log_scale = opts.log_scale;
            st.backend_mode = opts.native_backend;
            st.anim_zoom.set(opts.anim_zoom);
            st.anim_pan.set(opts.anim_pan);
            st.anim_scroll.set(opts.anim_scroll);
            st.anim_autoscale.set(opts.anim_autoscale);
            st.anim_live_bar.set(opts.anim_live_bar);
            st.set_intro(opts.anim_intro);
            st.glow_amount = opts.glow;
            st.candle_up = opts.up_color;
            st.candle_down = opts.down_color;
            st.chosen_style = opts.price_style.clone();
            st.scene.price_style = opts.price_style.clone();
            {
                let registry = st.chart_types.borrow();
                st.scene.base_painting = base_painting_of(&opts.price_style, &registry);
                st.scene.candle_override =
                    candle_override_for(&opts.price_style, &st.scene.style.chart_types, &registry);
            }
            if let Some(hook) = opts.price_axis_ticks {
                st.price_axis_ticks.set_hook(Some(hook));
            }
            if let Some(trigger) = opts.free_pan {
                st.apply_free_pan_trigger(trigger);
            }
        }
        // Seed a theme so get_config / apply_config work before mount (mount
        // overwrites it with the real theme). Candle colors follow the options.
        st.theme = derive_theme(&dark_theme(), &st.candle_up, &st.candle_down);
        st.coords.set_viewport(default_viewport());
        st
    }

    // ── queue and scheduler plumbing ──

    pub(crate) fn emit(&mut self, event: Event) {
        self.outbox.push(event);
    }

    /// `scheduler?.invalidate(level)`: a no-op until mounted.
    pub(crate) fn invalidate(&self, level: InvalidateLevel) {
        if self.mounted {
            self.sched.invalidate(level);
        }
    }

    pub(crate) fn text_of(&self, message: &crate::i18n::MessageLike) -> String {
        crate::i18n::text_of(self.i18n.as_deref(), message, &[])
    }

    pub(crate) fn set_intro(&mut self, next: IntroAnimation) {
        self.intro = next;
        if let Some(style) = next.style {
            self.intro_on_style = style;
        }
    }

    /// The theme the chrome paints with: the live theme pinned to the stable
    /// surface background, text and border.
    pub(crate) fn chrome_theme(&self) -> VelaTheme {
        let mut out = self.theme.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "background".into(),
                Value::String(self.surface_background.clone()),
            );
            obj.insert(
                "textColor".into(),
                Value::String(self.surface_text_color.clone()),
            );
            let border = if super::util::is_dark_color(&self.surface_background) {
                dark_theme()
            } else {
                crate::theme::light_theme()
            };
            if let Some(b) = border.get("borderColor") {
                obj.insert("borderColor".into(), b.clone());
            }
        }
        out
    }

    /// The colors the axis gutters paint with: the live chart background and its
    /// contrast-corrected text.
    pub(crate) fn axis_surface(&self) -> crate::render::chrome::chrome_renderer::AxisSurface {
        crate::render::chrome::chrome_renderer::AxisSurface {
            background: super::util::theme_string(&self.theme, "background"),
            text_color: super::util::theme_string(&self.theme, "textColor"),
        }
    }

    pub(crate) fn viewport(&self) -> ViewportState {
        self.coords.viewport()
    }
}
