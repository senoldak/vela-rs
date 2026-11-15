//! [`NativeHost`] on the real DOM: the browser half of the original's
//! `NativeRenderer` mount code (`mount`, `createPlotCanvas`, `syncSize`'s
//! reads, `canvasPile`, `syncLayerCanvasOrder`, the loading, attribution,
//! live-region and `shadeRight` elements).
//!
//! # DOM
//!
//! ```text
//! container (host-owned; carries the --vela-* layout variables)
//! └─ wrapper   position:relative, 100% x 100%, background, lang / dir
//!    ├─ plot   absolute, inset by the toolbar gutter on the left
//!    │  ├─ canvases of the pile (absolute, explicit CSS size, dir="ltr")
//!    │  ├─ .vela-shade-right
//!    │  └─ overlay root (plot-relative DOM overlays)
//!    ├─ .vela-attribution   the mark the project NOTICE requires
//!    ├─ .vela-loading       three pulsing dots, while loading
//!    └─ [aria-live]         the keyboard announcements
//! ```
//!
//! # What is real and what is not
//!
//! Real: `metrics`, `create_canvas`, `remove_canvas`, `order_layer_canvases`,
//! `create_slice_surface`, `request_frame`, `cancel_frame`, `frame_now_ms`,
//! `wall_ms`, `second_clock`, `set_background`, `apply_chrome_theme` (as
//! `--vela-chrome-*` custom properties), `apply_root_attributes`,
//! `publish_layout`, `set_loading_visible`, `attribution`, `announce`,
//! `shade_right`, `set_keyboard_enabled`, `focus`, `set_layout_mode`, `warn`, and
//! `drawings_ui` (the drawing toolbar, the inline text editor and the magnifier menu are
//! real, and so are the settings quick bar and panel, see [`crate::drawings_ui`]).
//!
//! Real through [`crate::chrome_ui`]: `mark_icons`, `scroll_button`, `mark_popover`, `layout_changed`,
//! `legend_providers_changed`, every `legend_*` member except `legend_set_values` (a no-op:
//! the rows carry no inline inputs), and `set_layout_mode`.
//!
//! Not this file's: the dialog members (`has_dialogs`, `open_settings_dialog`,
//! `open_indicator_settings`, `close_dialogs`, `settings_changed`) and `feature_changed`; the
//! latter should forward `indicatorTitles` / `indicatorValues` to
//! `ChromeUi::feature_changed`.
//!
//! # Re-entrancy
//!
//! The renderer calls these members while its state is borrowed, so nothing
//! here calls back into it. Frames run from the browser's own animation frame
//! callback, never from inside `request_frame`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

use vela_core::intl::TextDirection;
use vela_core::ports::chart_renderer::{
    IndicatorStatus, LayoutMode, RendererRootAttributes, VelaTheme, WallClock,
};
use vela_core::render::canvas::{Canvas2d, CanvasImage, RecordingCanvas};
use vela_core::render::chrome::marks_paint::MarkIcons;
use vela_core::render::core::scheduler::FrameTask;
use vela_core::render::drawings::DrawingsUiEvent;
use vela_core::render::drawings::indicator_slices::SliceSurface;
use vela_core::render::gl::Gl;
use vela_core::render::native::{
    AttributionState, CanvasSlot, HostCanvas, HostGlCanvas, HostMetrics, LayoutFacts,
    LegendPlotValue, LegendRow, MarkPopoverAction, NativeHost, NativeRenderer, ScrollButtonState,
};
use vela_core::runtime::{Platform, TimerId, Unsubscribe};
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, Document, Element, HtmlCanvasElement, HtmlElement};

use crate::canvas::BrowserCanvas;
use crate::chrome_ui::ChromeUi;
use crate::dialogs::{Dialogs, Tr};
use crate::dom::{
    Listener, create_element, global_document, remove_style, set_style, set_styles, vela_caf,
    vela_dpr, vela_now, vela_raf,
};
use crate::drawings_ui::DrawingsUi;
use crate::gl::{BrowserGlCanvas, webgl2_context_of};
use crate::logos::{VELA_LETTERS_SVG, VELA_MARK_SVG};
use vela_core::i18n::Translator;

/// Where the attribution mark links: the canonical project page.
pub const ATTRIBUTION_URL: &str = "https://velacharts.dev/?utm_source=vela&utm_medium=attribution";

/// The attribution mark's tooltip (`chart.attribution`; Vela's name is never translated).
const ATTRIBUTION_LABEL: (&str, &str) = ("chart.attribution", "Charting by Vela");

/// The data canvas's accessible name while keyboard control is on (the original's
/// `chart.a11y.keyboardHelp`).
const KEYBOARD_HELP: (&str, &str) = (
    "chart.a11y.keyboardHelp",
    "Interactive price chart. Use Left/Right arrows to move between bars, Shift+Arrow to pan, plus/minus to zoom, Home/End to jump, 0 to reset.",
);

/// Re-read the built-in mark's tooltip in `tr`'s language (the original's
/// `relabelAttributionMark`); a host's own mark keeps its words.
pub(crate) fn relabel_attribution_mark(el: &Element, tr: &Tr) {
    if !el
        .get_attribute("class")
        .is_some_and(|c| c.split_whitespace().any(|w| w == "vela-attribution"))
    {
        return;
    }
    let label = tr.t(ATTRIBUTION_LABEL.0, ATTRIBUTION_LABEL.1);
    let _ = el.set_attribute("title", &label);
    let _ = el.set_attribute("aria-label", &label);
}

const STYLE_ID: &str = "vela-host-styles";

/// The attribution mark's rules (the original's `vela-attribution-styles`) and
/// the loading dots' pulse.
const CSS: &str = r#"@layer vela.chrome {

.vela-attribution {
    text-decoration: none;
    display: flex;
    align-items: center;
    gap: 0;
}
.vela-attribution .vela-attr-symbol {
    flex: none;
    display: block;
    line-height: 0;
}
.vela-attribution .vela-attr-symbol svg {
    height: 24px;
    width: auto;
    display: block;
    filter: drop-shadow(0 1px 2px var(--vela-attr-shadow, rgba(0,0,0,0.45)));
}
.vela-attribution .vela-attr-wordmark {
    max-width: 0;
    overflow: hidden;
    opacity: 0;
    display: flex;
    align-items: center;
    transform: translateX(-8px);
    transition: max-width 0.3s ease, opacity 0.25s ease, transform 0.3s ease;
    flex: none;
}
.vela-attribution .vela-attr-wordmark svg {
    height: 24px;
    width: auto;
    display: block;
    filter: drop-shadow(0 1px 2px var(--vela-attr-shadow, rgba(0,0,0,0.45)));
}
.vela-attribution:hover .vela-attr-wordmark {
    max-width: 240px;
    opacity: 1;
    transform: translateX(0);
}
[data-vela-layout='mobile'] .vela-attribution .vela-attr-symbol svg,
[data-layout='mobile'] .vela-attribution .vela-attr-symbol svg { height: 20px; }
[data-vela-layout='mobile'] .vela-attribution .vela-attr-wordmark,
[data-layout='mobile'] .vela-attribution .vela-attr-wordmark { transform: translateX(-6px); }
[data-vela-layout='mobile'] .vela-attribution .vela-attr-wordmark svg,
[data-layout='mobile'] .vela-attribution .vela-attr-wordmark svg { height: 20px; }
[data-vela-layout='mobile'] .vela-attribution:hover .vela-attr-wordmark,
[data-layout='mobile'] .vela-attribution:hover .vela-attr-wordmark { transform: translateX(0); }

}
@keyframes vela-loading-pulse {
    from { opacity: 0.12; }
    to { opacity: 0.55; }
}
"#;

/// Time axis strip height the default attribution position assumes before the
/// first layout (the core's `TIME_AXIS_H`).
const DEFAULT_ATTRIBUTION_BOTTOM: f64 = 32.0;

// ── canvases ──

fn context_of(canvas: &HtmlCanvasElement) -> Rc<dyn Canvas2d> {
    match canvas.get_context("2d") {
        Ok(Some(ctx)) => {
            let ctx: CanvasRenderingContext2d = ctx.unchecked_into();
            Rc::new(BrowserCanvas::new(ctx))
        }
        // No 2D context (a headless stand-in): paint into a recording sink so the
        // renderer still runs.
        _ => Rc::new(RecordingCanvas::new()),
    }
}

/// One canvas of the pile.
pub struct BrowserHostCanvas {
    canvas: HtmlCanvasElement,
    ctx: Rc<dyn Canvas2d>,
    image: CanvasImage,
}

/// A canvas element placed like every canvas of the pile (absolute, whole plot, pinned left
/// to right), pointer-transparent unless it takes input.
fn pile_element(doc: &Document, pointer_events: bool) -> HtmlCanvasElement {
    let canvas: HtmlCanvasElement = create_element(doc, "canvas").unchecked_into();
    set_styles(
        &canvas,
        &[
            ("position", "absolute"),
            ("inset", "0"),
            ("width", "100%"),
            ("height", "100%"),
        ],
    );
    if !pointer_events {
        set_style(&canvas, "pointer-events", "none");
    }
    // The plot never mirrors: pinned left to right whatever the page direction.
    canvas.set_dir("ltr");
    canvas
}

/// A subscriber of the `Data` canvas element changing.
pub type DataWatcher = dyn Fn(&HtmlCanvasElement);

/// One slot of the pile: a 2D canvas, or the data canvas while it paints with WebGL2.
#[derive(Clone)]
enum PileCanvas {
    Plain(Rc<BrowserHostCanvas>),
    Gl(Rc<BrowserGlCanvas>),
}

impl PileCanvas {
    fn element(&self) -> &HtmlCanvasElement {
        match self {
            PileCanvas::Plain(c) => c.element(),
            PileCanvas::Gl(c) => c.element(),
        }
    }
}

impl BrowserHostCanvas {
    fn new(doc: &Document, pointer_events: bool) -> Rc<Self> {
        let canvas = pile_element(doc, pointer_events);
        let ctx = context_of(&canvas);
        let image: CanvasImage = Rc::new(JsValue::from(canvas.clone()));
        Rc::new(Self { canvas, ctx, image })
    }

    /// The `<canvas>` element.
    pub fn element(&self) -> &HtmlCanvasElement {
        &self.canvas
    }
}

impl HostCanvas for BrowserHostCanvas {
    fn context(&self) -> Rc<dyn Canvas2d> {
        self.ctx.clone()
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }

    fn set_size(&self, backing_width: f64, backing_height: f64, css_width: f64, css_height: f64) {
        // Assigning width / height clears the bitmap, as the renderer expects.
        self.canvas.set_width(backing_width as u32);
        self.canvas.set_height(backing_height as u32);
        set_styles(
            &self.canvas,
            &[
                ("width", &format!("{css_width}px")),
                ("height", &format!("{css_height}px")),
            ],
        );
    }
}

/// An offscreen canvas for a drawing slice.
struct BrowserSlice {
    canvas: HtmlCanvasElement,
    ctx: Rc<dyn Canvas2d>,
    image: CanvasImage,
}

impl SliceSurface for BrowserSlice {
    fn size(&self) -> (u32, u32) {
        (self.canvas.width(), self.canvas.height())
    }

    fn resize(&self, width: u32, height: u32) {
        self.canvas.set_width(width);
        self.canvas.set_height(height);
    }

    fn context(&self) -> Option<Rc<dyn Canvas2d>> {
        Some(self.ctx.clone())
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }
}

// ── the second-aligned wall clock ──

/// Headroom past the boundary so a timer that fires a hair early still lands in
/// the new second.
const BOUNDARY_SLACK_MS: f64 = 5.0;

type Subscribers = RefCell<Vec<(u64, Rc<RefCell<Box<dyn FnMut(f64)>>>)>>;

/// The original's `SecondClock`: a once-per-second pulse aligned to the wall
/// clock, running only while it has subscribers.
pub struct SecondClock {
    platform: Rc<dyn Platform>,
    subs: Subscribers,
    next: Cell<u64>,
    timer: Cell<Option<TimerId>>,
    me: Weak<SecondClock>,
}

impl SecondClock {
    pub fn new(platform: Rc<dyn Platform>) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            platform,
            subs: RefCell::new(Vec::new()),
            next: Cell::new(0),
            timer: Cell::new(None),
            me: me.clone(),
        })
    }

    /// Milliseconds from `now` to just past the next second boundary.
    pub fn delay_to_next_second(now: f64) -> f64 {
        let into_second = ((now % 1000.0) + 1000.0) % 1000.0;
        1000.0 - into_second + BOUNDARY_SLACK_MS
    }

    fn arm(&self) {
        let me = self.me.clone();
        let id = self.platform.set_timeout(
            Self::delay_to_next_second(self.platform.now_ms()),
            Box::new(move || {
                if let Some(clock) = me.upgrade() {
                    clock.fire();
                }
            }),
        );
        self.timer.set(Some(id));
    }

    fn fire(&self) {
        self.timer.set(None);
        let now = self.platform.now_ms();
        let subs: Vec<_> = self.subs.borrow().iter().map(|(_, s)| s.clone()).collect();
        for s in subs {
            if let Ok(mut f) = s.try_borrow_mut() {
                (**f)(now);
            }
        }
        if !self.subs.borrow().is_empty() && self.timer.get().is_none() {
            self.arm();
        }
    }
}

impl WallClock for SecondClock {
    fn on_tick(&self, cb: Box<dyn FnMut(f64)>) -> Unsubscribe {
        let id = self.next.get();
        self.next.set(id + 1);
        self.subs.borrow_mut().push((id, Rc::new(RefCell::new(cb))));
        if self.timer.get().is_none() {
            self.arm();
        }
        let me = self.me.clone();
        Box::new(move || {
            if let Some(clock) = me.upgrade() {
                clock.subs.borrow_mut().retain(|(i, _)| *i != id);
                if clock.subs.borrow().is_empty()
                    && let Some(timer) = clock.timer.take()
                {
                    clock.platform.clear_timeout(timer);
                }
            }
        })
    }
}

// ── the host ──

/// The ring of elements that is not a canvas: where the canvases slot in.
struct Skeleton {
    shade_right: Element,
    overlay_root: Element,
}

/// The browser's [`NativeHost`]. Build it with [`BrowserHost::new`] on a
/// container element; pass it to `NativeRenderer::new` as `Rc<dyn NativeHost>`.
pub struct BrowserHost {
    doc: Document,
    container: Element,
    wrapper: Element,
    plot: Element,
    skeleton: Skeleton,
    platform: Rc<dyn Platform>,
    clock: Rc<SecondClock>,
    canvases: RefCell<HashMap<CanvasSlot, PileCanvas>>,
    /// GL data canvases taken out of the pile. A canvas is replaced from inside its own
    /// `webglcontextrestored` handler (a failed restore), where dropping its listeners
    /// would free the closure that is running: it waits here until the next frame.
    retired: RefCell<Vec<Rc<BrowserGlCanvas>>>,
    /// The renderer, for the WebGL context events (set by `bind_renderer`).
    renderer: RefCell<Weak<NativeRenderer>>,
    /// Callbacks told when the `Data` canvas element is replaced (the input wiring follows it).
    data_watchers: RefCell<Vec<Weak<DataWatcher>>>,
    me: Weak<BrowserHost>,
    below: RefCell<Vec<String>>,
    above: RefCell<Vec<String>>,
    // frame loop
    frames: RefCell<BTreeMap<u32, FrameTask>>,
    next_frame: Cell<u32>,
    raf: RefCell<Option<JsValue>>,
    pump: Closure<dyn FnMut(JsValue)>,
    // geometry
    device_size: Cell<Option<(f64, f64)>>,
    toolbar_gutter: Cell<f64>,
    // presentation state
    shade_sig: Cell<Option<(f64, f64, f64)>>,
    chrome_text: RefCell<String>,
    attribution_el: RefCell<Option<Element>>,
    attribution_html: RefCell<Option<String>>,
    translator: RefCell<Option<Rc<dyn Translator>>>,
    loading_el: RefCell<Option<Element>>,
    live_region: RefCell<Option<Element>>,
    keyboard: Cell<bool>,
    layout_vars: RefCell<Vec<&'static str>>,
    /// The drawings widgets (toolbar, inline editor, settings bar, menus).
    ui: Rc<DrawingsUi>,
    /// The legend, pane controls, axis buttons, tooltips, scroll button and mark popup.
    chrome: Rc<ChromeUi>,
    /// The chart settings dialog, the indicator settings dialog and the gear.
    pub(crate) dialogs: Rc<Dialogs>,
}

impl BrowserHost {
    /// Build the wrapper and plot elements inside `container`. The container
    /// needs a size (the wrapper fills it); an empty box lays nothing out.
    pub fn new(container: &Element, platform: Rc<dyn Platform>) -> Result<Rc<Self>, JsValue> {
        let doc = container
            .owner_document()
            .or_else(global_document)
            .ok_or_else(|| JsValue::from_str("vela: no document to build the chart in"))?;
        ensure_styles(&doc);

        let wrapper = create_element(&doc, "div");
        set_styles(
            &wrapper,
            &[
                ("position", "relative"),
                ("width", "100%"),
                ("height", "100%"),
                ("overflow", "hidden"),
                ("cursor", "crosshair"),
                ("user-select", "none"),
                ("-webkit-user-select", "none"),
            ],
        );
        wrapper.set_class_name("vela-native");

        let plot = create_element(&doc, "div");
        set_styles(
            &plot,
            &[
                ("position", "absolute"),
                ("top", "0"),
                ("right", "0"),
                ("bottom", "0"),
                ("left", "0"),
            ],
        );

        // The `shadeRight` veil's styleable twin, just under the cursor canvas.
        let shade_right = create_element(&doc, "div");
        shade_right.set_class_name("vela-shade-right");
        let _ = shade_right.set_attribute("aria-hidden", "true");
        let _ = shade_right.set_attribute("hidden", "");
        set_styles(
            &shade_right,
            &[
                ("position", "absolute"),
                ("top", "0"),
                ("left", "0"),
                ("width", "0"),
                ("height", "0"),
                ("pointer-events", "none"),
                ("display", "none"),
            ],
        );
        let overlay_root = create_element(&doc, "div");
        overlay_root.set_class_name("vela-overlay-root");
        set_styles(
            &overlay_root,
            &[
                ("position", "absolute"),
                ("inset", "0"),
                ("pointer-events", "none"),
            ],
        );
        plot.append_child(&shade_right)?;
        plot.append_child(&overlay_root)?;
        wrapper.append_child(&plot)?;

        let clock = SecondClock::new(platform.clone());
        let ui = DrawingsUi::new(&doc, &wrapper, &plot, platform.clone());
        let chrome = ChromeUi::new(&doc, &wrapper, &plot, &overlay_root, platform.clone());
        let dialogs = Dialogs::new(&doc, container, &wrapper, &plot, platform.clone());
        let host = Rc::new_cyclic(|me: &Weak<BrowserHost>| {
            let weak = me.clone();
            let pump = Closure::<dyn FnMut(JsValue)>::new(move |_timestamp: JsValue| {
                if let Some(host) = weak.upgrade() {
                    host.run_frames();
                }
            });
            BrowserHost {
                doc: doc.clone(),
                container: container.clone(),
                wrapper: wrapper.clone(),
                plot: plot.clone(),
                skeleton: Skeleton {
                    shade_right,
                    overlay_root,
                },
                platform,
                clock,
                canvases: RefCell::new(HashMap::new()),
                retired: RefCell::new(Vec::new()),
                renderer: RefCell::new(Weak::new()),
                data_watchers: RefCell::new(Vec::new()),
                me: me.clone(),
                below: RefCell::new(Vec::new()),
                above: RefCell::new(Vec::new()),
                frames: RefCell::new(BTreeMap::new()),
                next_frame: Cell::new(1),
                raf: RefCell::new(None),
                pump,
                device_size: Cell::new(None),
                toolbar_gutter: Cell::new(0.0),
                shade_sig: Cell::new(None),
                chrome_text: RefCell::new("#b2b5be".to_string()),
                attribution_el: RefCell::new(None),
                attribution_html: RefCell::new(None),
                translator: RefCell::new(None),
                loading_el: RefCell::new(None),
                live_region: RefCell::new(None),
                keyboard: Cell::new(false),
                layout_vars: RefCell::new(Vec::new()),
                ui,
                chrome,
                dialogs,
            }
        });
        // The mark exists from the start (the NOTICE file requires it): the renderer
        // refines its position and ink as layout and theme settle.
        host.attribution(&AttributionState {
            enabled: true,
            html: None,
            left_px: 12.0,
            bottom_px: DEFAULT_ATTRIBUTION_BOTTOM,
            ink: "#ffffff".to_string(),
        });
        container.append_child(&wrapper)?;
        // A press on an indicator dialog's scrim drops the legend's row selection
        // (the original's `onBackdropClose` -> `clearSelection`).
        let legend = Rc::downgrade(host.chrome());
        host.dialogs.set_on_backdrop_close(Some(Rc::new(move || {
            if let Some(chrome) = legend.upgrade() {
                chrome.clear_selection();
            }
        })));
        Ok(host)
    }

    // ── what the input wiring and the chart need ──

    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub fn container(&self) -> &Element {
        &self.container
    }

    pub fn wrapper(&self) -> &Element {
        &self.wrapper
    }

    /// The plot element (inset by the toolbar gutter).
    pub fn plot(&self) -> &Element {
        &self.plot
    }

    /// The data canvas, once the renderer has mounted: the element that takes
    /// pointer, wheel and key input.
    pub fn data_canvas(&self) -> Option<HtmlCanvasElement> {
        self.canvases
            .borrow()
            .get(&CanvasSlot::Data)
            .map(|c| c.element().clone())
    }

    /// Be told, with the new element, whenever the `Data` canvas is replaced (a WebGL
    /// canvas given up for a 2D one, or the reverse at a re-mount). The callback runs
    /// from inside a host call, so it may only touch the DOM, never the renderer. Held
    /// weakly: dropping the `Rc` ends the subscription.
    pub fn watch_data_canvas(&self, callback: Weak<DataWatcher>) {
        self.data_watchers.borrow_mut().push(callback);
    }

    /// The `Data` canvas is a WebGL2 canvas right now (the geometry backend paints with GL).
    pub fn data_canvas_is_gl(&self) -> bool {
        matches!(
            self.canvases.borrow().get(&CanvasSlot::Data),
            Some(PileCanvas::Gl(_))
        )
    }

    /// What a new `Data` canvas needs besides the pile's placement: the keyboard
    /// attributes when the feature is on, and the watchers' news.
    fn data_canvas_arrived(&self) {
        let Some(el) = self.data_canvas() else {
            return;
        };
        if self.keyboard.get() {
            self.mark_keyboard_target(&el);
        }
        let watchers: Vec<Rc<DataWatcher>> = {
            let mut list = self.data_watchers.borrow_mut();
            list.retain(|w| w.strong_count() > 0);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        for watcher in watchers {
            watcher(&el);
        }
    }

    /// Take `slot` out of the pile and the plot. A GL canvas is parked in `retired` (see there).
    fn take_canvas(&self, slot: &CanvasSlot) {
        let old = self.canvases.borrow_mut().remove(slot);
        match old {
            Some(PileCanvas::Plain(c)) => c.element().remove(),
            Some(PileCanvas::Gl(c)) => {
                c.element().remove();
                // A backend that was destroyed has released its context already; one whose
                // program never built has not (browsers cap live contexts per page).
                let gl = c.browser_gl();
                if !gl.is_context_lost() {
                    gl.lose_context();
                }
                self.retired.borrow_mut().push(c);
                self.arm(); // the next frame lets the retired canvas go
            }
            None => {}
        }
    }

    fn mark_keyboard_target(&self, data: &HtmlCanvasElement) {
        data.set_tab_index(0);
        let _ = data.set_attribute("role", "application");
        let _ = data.set_attribute("aria-label", &self.keyboard_help());
    }

    fn keyboard_help(&self) -> String {
        self.tr().t(KEYBOARD_HELP.0, KEYBOARD_HELP.1)
    }

    fn tr(&self) -> Tr {
        Tr::new(self.translator.borrow().clone())
    }

    /// The chart's translator: the attribution tooltip and the keyboard help read in its
    /// language at once.
    pub fn set_translator(&self, translator: Option<Rc<dyn Translator>>) {
        *self.translator.borrow_mut() = translator;
        let tr = self.tr();
        if let Some(el) = self.attribution_el.borrow().as_ref() {
            relabel_attribution_mark(el, &tr);
        }
        if self.keyboard.get()
            && let Some(data) = self.data_canvas()
        {
            let _ = data.set_attribute("aria-label", &self.keyboard_help());
        }
    }

    /// The WebGL context events of the data canvas `canvas`: from the browser's event
    /// loop, so calling into the renderer is allowed here.
    fn gl_context_event(&self, canvas: &HtmlCanvasElement, restored: bool, event: &JsValue) {
        if !restored {
            // Keeps the context restorable.
            event.unchecked_ref::<web_sys::Event>().prevent_default();
        }
        // A canvas the shell has already replaced has nothing to say to the renderer.
        let current = self
            .data_canvas()
            .is_some_and(|d| JsValue::from(d) == JsValue::from(canvas.clone()));
        if !current {
            return;
        }
        let renderer = self.renderer.borrow().upgrade();
        if let Some(renderer) = renderer {
            if restored {
                renderer.gl_context_restored();
            } else {
                renderer.gl_context_lost();
            }
        }
    }

    /// Every canvas of the pile now in the plot (the data canvas included).
    pub fn pile_elements(&self) -> Vec<(CanvasSlot, HtmlCanvasElement)> {
        self.canvases
            .borrow()
            .iter()
            .map(|(slot, c)| (slot.clone(), c.element().clone()))
            .collect()
    }

    /// Tell the drawings widgets which renderer they act on. Call once after the renderer
    /// is built; widgets paint events that arrive earlier, they only need the renderer's
    /// controller once the user acts.
    pub fn bind_renderer(&self, renderer: &Rc<NativeRenderer>) {
        *self.renderer.borrow_mut() = Rc::downgrade(renderer);
        self.ui.bind(renderer);
        self.chrome.bind(renderer);
    }

    /// The in-chart chrome (legend, pane controls, tooltips, ...): the owner of the chart
    /// gives it the translator and the mark source through this.
    pub fn chrome(&self) -> &Rc<ChromeUi> {
        &self.chrome
    }

    /// The drawing toolbar, inline editor, settings quick bar and panel.
    pub fn drawings_ui(&self) -> &Rc<DrawingsUi> {
        &self.ui
    }

    /// Whether the renderer's `keyboard` feature is on.
    pub fn keyboard_enabled(&self) -> bool {
        self.keyboard.get()
    }

    /// The device-pixel box the `ResizeObserver` reported for the plot.
    pub fn set_device_size(&self, size: Option<(f64, f64)>) {
        self.device_size.set(size);
    }

    /// Remove everything the host put in the page. Call after the renderer's
    /// `destroy` (which already removed the canvases).
    pub fn dispose(&self) {
        self.dialogs.dispose();
        self.ui.dispose();
        self.chrome.dispose();
        self.canvases.borrow_mut().clear();
        self.retired.borrow_mut().clear();
        self.data_watchers.borrow_mut().clear();
        if let Some(handle) = self.raf.borrow_mut().take() {
            vela_caf(&handle);
        }
        self.frames.borrow_mut().clear();
        self.wrapper.remove();
        for name in self.layout_vars.borrow_mut().drain(..) {
            remove_style(&self.container, name);
        }
        let _ = self.container.remove_attribute("data-vela-layout");
    }

    // ── frame loop ──

    fn arm(&self) {
        if self.raf.borrow().is_some() {
            return;
        }
        let handle = vela_raf(self.pump.as_ref().unchecked_ref());
        *self.raf.borrow_mut() = Some(handle);
    }

    /// The animation frame: run the tasks armed at entry in order (a task armed
    /// by one of them waits for the next frame), holding no borrow while they run.
    fn run_frames(&self) {
        self.raf.borrow_mut().take();
        self.retired.borrow_mut().clear();
        let due: Vec<u32> = self.frames.borrow().keys().copied().collect();
        for id in due {
            let task = self.frames.borrow_mut().remove(&id);
            if let Some(task) = task {
                task();
            }
        }
        if !self.frames.borrow().is_empty() {
            self.arm();
        }
    }

    // ── DOM order ──

    /// Where `slot` sits in the pile, back to front, for the slots the host
    /// knows by position (a layer is slotted by `order_layer_canvases`).
    fn fixed_order() -> [Option<CanvasSlot>; 9] {
        [
            Some(CanvasSlot::Backdrop),
            Some(CanvasSlot::Data),
            Some(CanvasSlot::Volume),
            Some(CanvasSlot::Vpvr),
            Some(CanvasSlot::Chrome),
            Some(CanvasSlot::Drawings),
            None, // the shadeRight twin
            Some(CanvasSlot::Cursor),
            None, // the overlay root
        ]
    }

    /// The element a canvas of `slot` goes in front of: the next one that is
    /// already in the plot.
    fn next_sibling_of(&self, slot: &CanvasSlot) -> Option<Element> {
        let canvases = self.canvases.borrow();
        let order = Self::fixed_order();
        let start = match slot {
            // A layer's channel is only known by order_layer_canvases; until then it
            // goes over the vpvr canvas, under the chrome.
            CanvasSlot::Layer(_) => order
                .iter()
                .position(|s| *s == Some(CanvasSlot::Chrome))
                .unwrap_or(0),
            other => order.iter().position(|s| s.as_ref() == Some(other))? + 1,
        };
        for (i, entry) in order.iter().enumerate().skip(start) {
            match entry {
                Some(s) => {
                    if let Some(c) = canvases.get(s) {
                        return Some(c.element().clone().into());
                    }
                }
                None if i == 6 => return Some(self.skeleton.shade_right.clone()),
                None => return Some(self.skeleton.overlay_root.clone()),
            }
        }
        Some(self.skeleton.overlay_root.clone())
    }

    fn layer_element(&self, channel: &str) -> Option<Element> {
        self.canvases
            .borrow()
            .get(&CanvasSlot::Layer(channel.to_string()))
            .map(|c| c.element().clone().into())
    }

    fn element_of(&self, slot: &CanvasSlot) -> Option<Element> {
        self.canvases
            .borrow()
            .get(slot)
            .map(|c| c.element().clone().into())
    }
}

fn ensure_styles(doc: &Document) {
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let style = create_element(doc, "style");
    style.set_id(STYLE_ID);
    style.set_text_content(Some(CSS));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    } else if let Some(root) = doc.document_element() {
        let _ = root.append_child(&style);
    }
}

fn kebab(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn is_dark_ink(ink: &str) -> bool {
    ink.eq_ignore_ascii_case("#ffffff")
}

/// The attribution element: an anchor around the Morning Star V, the rest of the wordmark
/// revealed on hover (`createAttributionMark`). Positioned absolutely by the caller.
pub(crate) fn build_attribution_mark(doc: &Document, tr: &Tr) -> Element {
    let a = create_element(doc, "a");
    a.set_class_name("vela-attribution");
    let _ = a.set_attribute("href", ATTRIBUTION_URL);
    let _ = a.set_attribute("target", "_blank");
    let _ = a.set_attribute("rel", "noopener");
    relabel_attribution_mark(&a, tr);
    let _ = a.set_attribute("data-vela-screenshot", "1");
    set_styles(
        &a,
        &[
            ("position", "absolute"),
            ("z-index", "6"),
            ("pointer-events", "auto"),
            ("cursor", "pointer"),
        ],
    );
    let symbol = create_element(doc, "span");
    symbol.set_class_name("vela-attr-symbol");
    let _ = symbol.set_attribute("aria-hidden", "true");
    symbol.set_inner_html(VELA_MARK_SVG);
    let wordmark = create_element(doc, "span");
    wordmark.set_class_name("vela-attr-wordmark");
    wordmark.set_inner_html(VELA_LETTERS_SVG);
    let _ = a.append_child(&symbol);
    let _ = a.append_child(&wordmark);
    a
}

impl BrowserHost {
    /// Build the attribution element: an anchor around the Morning Star V, the
    /// rest of the wordmark revealed on hover (`createAttributionMark`).
    fn build_mark(&self) -> Element {
        build_attribution_mark(&self.doc, &self.tr())
    }

    /// A host-supplied mark shown instead of the built-in one. The string is
    /// DEVELOPER-supplied markup (`attribution: '<html>'`), never user input.
    fn build_custom_mark(&self, html: &str) -> Element {
        let el = create_element(&self.doc, "div");
        el.set_class_name("vela-attribution-custom");
        set_styles(
            &el,
            &[
                ("position", "absolute"),
                ("z-index", "6"),
                ("pointer-events", "auto"),
                ("display", "flex"),
                ("align-items", "center"),
                ("line-height", "1"),
            ],
        );
        el.set_inner_html(html);
        let _ = el.set_attribute("data-vela-screenshot", "1");
        el
    }

    fn set_layout_var(&self, name: &'static str, value: f64) {
        set_style(&self.container, name, &format!("{value}px"));
        let mut vars = self.layout_vars.borrow_mut();
        if !vars.contains(&name) {
            vars.push(name);
        }
    }
}

impl NativeHost for BrowserHost {
    // ── geometry ──

    fn metrics(&self) -> HostMetrics {
        let wrapper = self.wrapper.unchecked_ref::<HtmlElement>();
        let rect = self.plot.get_bounding_client_rect();
        HostMetrics {
            client_width: f64::from(wrapper.client_width()),
            client_height: f64::from(wrapper.client_height()),
            plot_width: rect.width(),
            plot_height: rect.height(),
            dpr: vela_dpr(),
            device_size: self.device_size.get(),
        }
    }

    // ── the canvas pile ──

    fn create_canvas(&self, slot: &CanvasSlot) -> Rc<dyn HostCanvas> {
        // Replacing a slot (a re-mount) drops the old element first.
        self.take_canvas(slot);
        let canvas = BrowserHostCanvas::new(&self.doc, *slot == CanvasSlot::Data);
        let before = self.next_sibling_of(slot);
        let el: &Element = canvas.element();
        let _ = self.plot.insert_before(el, before.as_deref());
        self.canvases
            .borrow_mut()
            .insert(slot.clone(), PileCanvas::Plain(canvas.clone()));
        if *slot == CanvasSlot::Data {
            self.data_canvas_arrived();
        }
        canvas
    }

    fn create_gl_canvas(&self) -> Option<Rc<dyn HostGlCanvas>> {
        let element = pile_element(&self.doc, true);
        // `None` is "no WebGL2 here": the shell keeps (or makes) the 2D canvas.
        let ctx = webgl2_context_of(&element)?;
        self.take_canvas(&CanvasSlot::Data);
        let listeners: Vec<Listener> =
            [("webglcontextlost", false), ("webglcontextrestored", true)]
                .into_iter()
                .map(|(name, restored)| {
                    let me = self.me.clone();
                    let canvas = element.clone();
                    let target: &web_sys::EventTarget = element.as_ref();
                    Listener::new(target, name, false, false, move |event: JsValue| {
                        if let Some(host) = me.upgrade() {
                            host.gl_context_event(&canvas, restored, &event);
                        }
                    })
                })
                .collect();
        let canvas = BrowserGlCanvas::new(element, ctx, listeners);
        let before = self.next_sibling_of(&CanvasSlot::Data);
        let el: &Element = canvas.element();
        let _ = self.plot.insert_before(el, before.as_deref());
        self.canvases
            .borrow_mut()
            .insert(CanvasSlot::Data, PileCanvas::Gl(canvas.clone()));
        self.data_canvas_arrived();
        Some(canvas)
    }

    fn remove_canvas(&self, slot: &CanvasSlot) {
        self.take_canvas(slot);
        if let CanvasSlot::Layer(channel) = slot {
            self.below.borrow_mut().retain(|c| c != channel);
            self.above.borrow_mut().retain(|c| c != channel);
        }
    }

    fn order_layer_canvases(&self, below: &[String], above: &[String]) {
        *self.below.borrow_mut() = below.to_vec();
        *self.above.borrow_mut() = above.to_vec();
        // Re-inserting an absolutely positioned, pointer-transparent canvas repaints
        // nothing by itself; the data canvas (which holds pointer capture) is never moved.
        let data = self.element_of(&CanvasSlot::Data);
        let chrome = self.element_of(&CanvasSlot::Chrome);
        for channel in below {
            if let Some(el) = self.layer_element(channel) {
                let _ = self.plot.insert_before(&el, data.as_deref());
            }
        }
        for channel in above {
            if let Some(el) = self.layer_element(channel) {
                let _ = self.plot.insert_before(&el, chrome.as_deref());
            }
        }
    }

    fn create_slice_surface(&self) -> Rc<dyn SliceSurface> {
        let canvas: HtmlCanvasElement = create_element(&self.doc, "canvas").unchecked_into();
        let ctx = context_of(&canvas);
        let image: CanvasImage = Rc::new(JsValue::from(canvas.clone()));
        Rc::new(BrowserSlice { canvas, ctx, image })
    }

    // ── frame loop and clocks ──

    fn request_frame(&self, task: FrameTask) -> u32 {
        let id = self.next_frame.get();
        self.next_frame.set(id.checked_add(1).unwrap_or(1));
        self.frames.borrow_mut().insert(id, task);
        self.arm();
        id
    }

    fn cancel_frame(&self, handle: u32) {
        let empty = {
            let mut frames = self.frames.borrow_mut();
            frames.remove(&handle);
            frames.is_empty()
        };
        if empty && let Some(raf) = self.raf.borrow_mut().take() {
            vela_caf(&raf);
        }
    }

    fn frame_now_ms(&self) -> f64 {
        vela_now()
    }

    fn wall_ms(&self) -> f64 {
        js_sys::Date::now()
    }

    fn second_clock(&self) -> Option<Rc<dyn WallClock>> {
        let clock: Rc<dyn WallClock> = self.clock.clone();
        Some(clock)
    }

    // ── presentation effects ──

    fn set_background(&self, css_color: &str) {
        set_style(&self.wrapper, "background", css_color);
    }

    /// The chrome theme as custom properties on the wrapper, one per string
    /// field (`textColor` becomes `--vela-chrome-text-color`). The kit that
    /// styles the original's dialogs and legend is a later stage; these are what
    /// that chrome (and a host's own CSS) can read meanwhile.
    fn apply_chrome_theme(&self, theme: &VelaTheme) {
        if let Some(fields) = theme.as_object() {
            for (key, value) in fields {
                if let Some(text) = value.as_str() {
                    set_style(
                        &self.wrapper,
                        &format!("--vela-chrome-{}", kebab(key)),
                        text,
                    );
                    if key == "textColor" {
                        *self.chrome_text.borrow_mut() = text.to_string();
                    }
                }
            }
        }
        self.chrome.theme_changed();
    }

    fn apply_root_attributes(&self, attrs: &RendererRootAttributes) {
        match &attrs.lang {
            Some(lang) if !lang.is_empty() => {
                let _ = self.wrapper.set_attribute("lang", lang);
            }
            _ => {
                let _ = self.wrapper.remove_attribute("lang");
            }
        }
        match attrs.dir {
            Some(TextDirection::Ltr) => {
                let _ = self.wrapper.set_attribute("dir", "ltr");
            }
            Some(TextDirection::Rtl) => {
                let _ = self.wrapper.set_attribute("dir", "rtl");
            }
            None => {
                let _ = self.wrapper.remove_attribute("dir");
            }
        }
    }

    fn publish_layout(&self, layout: &LayoutFacts) {
        // The plot sits to the right of the toolbar gutter; everything inside it
        // stays 0-based at the plot's left edge.
        if self.toolbar_gutter.get() != layout.toolbar_gutter {
            self.toolbar_gutter.set(layout.toolbar_gutter);
            set_style(&self.plot, "left", &format!("{}px", layout.toolbar_gutter));
        }
        self.set_layout_var("--vela-toolbar-gutter", layout.toolbar_gutter);
        self.set_layout_var("--vela-scale-gutter", layout.scale_gutter);
        if let Some(v) = layout.bottom_gutter {
            self.set_layout_var("--vela-bottom-gutter", v);
        }
        if let Some(v) = layout.price_pane_top {
            self.set_layout_var("--vela-price-pane-top", v);
        }
        if let Some(v) = layout.price_pane_bottom {
            self.set_layout_var("--vela-price-pane-bottom", v);
        }
    }

    fn set_loading_visible(&self, visible: bool) {
        if !visible {
            if let Some(el) = self.loading_el.borrow_mut().take() {
                el.remove();
            }
            return;
        }
        if self.loading_el.borrow().is_some() {
            return;
        }
        // Built fresh per show so it picks up the current theme.
        let el = create_element(&self.doc, "div");
        el.set_class_name("vela-loading");
        set_styles(
            &el,
            &[
                ("position", "absolute"),
                ("inset", "0"),
                ("display", "flex"),
                ("align-items", "center"),
                ("justify-content", "center"),
                ("gap", "7px"),
                ("pointer-events", "none"),
            ],
        );
        let ink = self.chrome_text.borrow().clone();
        for i in 0..3 {
            let dot = create_element(&self.doc, "span");
            set_styles(
                &dot,
                &[
                    ("width", "7px"),
                    ("height", "7px"),
                    ("border-radius", "50%"),
                    ("background", &ink),
                    ("opacity", "0.15"),
                    // Staggered phases via negative delays: every dot animates from the first frame.
                    (
                        "animation",
                        &format!(
                            "vela-loading-pulse 800ms ease-in-out {}ms infinite alternate",
                            -i * 260
                        ),
                    ),
                ],
            );
            let _ = el.append_child(&dot);
        }
        let _ = self.wrapper.append_child(&el); // last: above the canvases
        *self.loading_el.borrow_mut() = Some(el);
    }

    fn attribution(&self, state: &AttributionState) {
        let rebuild = {
            let current = self.attribution_html.borrow();
            self.attribution_el.borrow().is_none() || *current != state.html
        };
        if rebuild {
            if let Some(old) = self.attribution_el.borrow_mut().take() {
                old.remove();
            }
            let el = match &state.html {
                Some(html) => self.build_custom_mark(html),
                None => self.build_mark(),
            };
            let _ = self.wrapper.append_child(&el);
            *self.attribution_el.borrow_mut() = Some(el);
            *self.attribution_html.borrow_mut() = state.html.clone();
        }
        let guard = self.attribution_el.borrow();
        let Some(el) = guard.as_ref() else { return };
        set_style(el, "display", if state.enabled { "" } else { "none" });
        set_styles(
            el,
            &[
                ("left", &format!("{}px", state.left_px)),
                ("bottom", &format!("{}px", state.bottom_px)),
                ("color", &state.ink),
            ],
        );
        if state.html.is_none() {
            // The shadow flips with the ink: a black mark under a black shadow reads as mud.
            let shadow = if is_dark_ink(&state.ink) {
                "rgba(0,0,0,0.45)"
            } else {
                "rgba(255,255,255,0.55)"
            };
            set_style(el, "--vela-attr-shadow", shadow);
        }
    }

    fn announce(&self, text: &str) {
        if let Some(region) = self.live_region.borrow().as_ref() {
            region.set_text_content(Some(text));
        }
    }

    fn shade_right(&self, area: Option<(f64, f64, f64)>) {
        if self.shade_sig.get() == area {
            return;
        }
        self.shade_sig.set(area);
        let el = &self.skeleton.shade_right;
        match area {
            None => {
                let _ = el.set_attribute("hidden", "");
                // An authored `display` on the hook class must not defeat [hidden].
                set_style(el, "display", "none");
            }
            Some((x, width, height)) => {
                set_styles(
                    el,
                    &[
                        ("left", &format!("{x}px")),
                        ("width", &format!("{width}px")),
                        ("height", &format!("{height}px")),
                        ("display", ""),
                    ],
                );
                let _ = el.remove_attribute("hidden");
            }
        }
    }

    fn set_keyboard_enabled(&self, enabled: bool) {
        self.keyboard.set(enabled);
        let Some(data) = self.data_canvas() else {
            return;
        };
        if enabled {
            data.set_tab_index(0);
            let _ = data.set_attribute("role", "application");
            let _ = data.set_attribute("aria-label", &self.keyboard_help());
            if self.live_region.borrow().is_none() {
                let region = create_element(&self.doc, "div");
                let _ = region.set_attribute("aria-live", "polite");
                let _ = region.set_attribute("role", "status");
                // Visually hidden, but available to assistive tech.
                set_styles(
                    &region,
                    &[
                        ("position", "absolute"),
                        ("width", "1px"),
                        ("height", "1px"),
                        ("margin", "-1px"),
                        ("padding", "0"),
                        ("overflow", "hidden"),
                        ("clip", "rect(0 0 0 0)"),
                        ("white-space", "nowrap"),
                        ("border", "0"),
                    ],
                );
                let _ = self.wrapper.append_child(&region);
                *self.live_region.borrow_mut() = Some(region);
            }
        } else {
            let _ = data.remove_attribute("tabindex");
            let _ = data.remove_attribute("role");
            let _ = data.remove_attribute("aria-label");
            if let Some(region) = self.live_region.borrow_mut().take() {
                region.remove();
            }
        }
    }

    fn focus(&self) {
        if let Some(data) = self.data_canvas() {
            let _ = data.unchecked_ref::<HtmlElement>().focus();
        }
    }

    fn warn(&self, message: &str) {
        self.platform.warn(message);
    }

    fn set_layout_mode(&self, mode: LayoutMode) {
        let value = match mode {
            LayoutMode::Mobile => "mobile",
            LayoutMode::Desktop => "desktop",
        };
        let _ = self.container.set_attribute("data-vela-layout", value);
        self.chrome.set_layout_mode(mode);
        // An open indicator dialog is re-presented for the new layout.
        self.dialogs.layout_changed();
    }

    fn drawings_ui(&self, event: &DrawingsUiEvent) {
        self.ui.handle(event);
    }

    // ── the dialogs (see `crate::dialogs`) ──

    fn has_dialogs(&self) -> bool {
        self.dialogs.available()
    }

    fn open_settings_dialog(&self, section: Option<&str>) {
        self.dialogs.open_settings(section);
    }

    fn open_indicator_settings(&self, indicator_id: &str) {
        self.dialogs.open_indicator(indicator_id);
    }

    fn close_dialogs(&self) {
        self.dialogs.close_all();
    }

    fn settings_changed(&self) {
        self.dialogs.settings_changed();
    }

    fn feature_changed(&self, key: &str, value: &serde_json::Value) {
        self.dialogs.feature_changed(key, value);
        // `indicatorTitles` / `indicatorValues` restyle the legend.
        self.chrome.feature_changed(key, value);
    }

    // ── the chrome widgets (see `crate::chrome_ui`) ──

    fn mark_icons(&self) -> Option<Rc<dyn MarkIcons>> {
        let icons: Rc<dyn MarkIcons> = self.chrome.mark_icons();
        Some(icons)
    }

    fn scroll_button(&self, state: &ScrollButtonState) {
        self.chrome.scroll_button_state(state);
    }

    fn mark_popover(&self, action: &MarkPopoverAction) {
        self.chrome.mark_popover_action(action);
    }

    fn legend_upsert(&self, row: &LegendRow) {
        self.chrome.legend_upsert(row);
    }

    fn legend_remove(&self, id: &str) {
        self.chrome.legend_remove(id);
        // A removed indicator's settings dialog closes with its row.
        self.dialogs.indicator_removed(id);
    }

    // `legend_set_values` stays the default no-op: the rows carry no inline inputs (the
    // settings dialog reads the model when it opens).

    fn legend_set_status(&self, id: &str, status: IndicatorStatus) {
        self.chrome.legend_set_status(id, status);
    }

    fn legend_set_pane(&self, id: &str, pane_id: &str) {
        self.chrome.legend_set_pane(id, pane_id);
    }

    fn legend_set_visible(&self, id: &str, visible: bool) {
        self.chrome.legend_set_visible(id, visible);
    }

    fn legend_relabel(&self, titles: &[(String, String)]) {
        self.chrome.legend_relabel(titles);
    }

    fn legend_order(&self, ids: &[String]) {
        self.chrome.legend_order(ids);
    }

    fn legend_collapsed_panes(&self, masters: &[(String, Option<String>)]) {
        self.chrome.legend_collapsed(masters);
    }

    fn legend_plot_values(&self, values: &[(String, Vec<LegendPlotValue>)]) {
        self.chrome.legend_plot_values(values);
    }

    fn layout_changed(&self) {
        self.chrome.layout_changed();
    }

    fn legend_providers_changed(&self) {
        self.chrome.legend_providers_changed();
    }
}
