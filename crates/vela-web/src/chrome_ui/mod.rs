//! The DOM chrome the renderer leaves to its host, driven by the
//! [`NativeHost`](vela_core::render::native::NativeHost) seam: the indicator LEGEND, the
//! per-pane hover controls, the A / L axis-scale buttons, the label and mark tooltips, the
//! scroll-to-latest button and the timeline-mark popup.
//!
//! | widget | module | the original |
//! |---|---|---|
//! | legend rows, fold chip, row menus, callouts | [`legend`] | `InputsUI.ts` |
//! | pane hover cluster, A / L buttons | [`panes`] | `PaneControls.ts`, `AxisScaleButtons.ts` |
//! | tooltips | [`tooltip`] | `chrome-tooltip.ts`, `LabelTooltip.ts` |
//! | scroll-to-latest button | [`scroll`] | `NativeRenderer.makeScrollToRealtimeButton` |
//! | mark popup, mark icons | [`marks`], [`mark_icons`] | `MarkPopover.ts`, `MarkIconRaster.ts` |
//!
//! They are built WITHOUT the original's `vela-ui` kit: native elements styled from the
//! `--vela-chrome-*` properties the host writes on the wrapper and the `--vela-lg-*`
//! properties this module writes on its root from the renderer's live plot theme.
//!
//! # DOM
//!
//! One root (`.vela-chrome-ui`, absolute over the plot, pointer-transparent) holds
//! everything; every control inside re-enables its own pointer events.
//!
//! # Re-entrancy
//!
//! The renderer calls the host WHILE its state is borrowed. Members here therefore only
//! write DOM and queue work: whatever needs a pull query (`pane_views`, `indicator_model`,
//! the legend providers) is flagged dirty and runs from one animation frame
//! ([`ChromeUi::flush`]). DOM handlers call the renderer directly (they run on a fresh
//! stack), holding none of this module's borrows while they do.
//!
//! # Closures and teardown
//!
//! A row's close button removes the row from inside its own click handler. A `Closure` must
//! not be freed while it runs, so removed widgets are [`retire`](ChromeUi::retire)d: the
//! element is gone at once and the listeners are freed from a timer, on a fresh stack.
//! [`dispose`](ChromeUi::dispose) frees everything.
//!
//! # Strings
//!
//! Every string carries the original's message id and English default and resolves through
//! the translator given to [`ChromeUi::set_translator`]; without one (the seam does not hand
//! the host the chart's translator) it is English.

mod icons;
mod legend;
mod mark_icons;
mod marks;
mod menu;
mod panes;
mod popover;
pub mod sanitize;
mod scroll;
mod style;
mod tooltip;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::i18n::{MessageArg, MessageLike, Translator, text_of};
use vela_core::marks::TimelineMark;
use vela_core::ports::chart_renderer::LayoutMode;
use vela_core::render::native::NativeRenderer;
use vela_core::runtime::{Platform, TimerId};
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element};

use crate::dom::{Listener, get_f64, get_str, make, set_style, vela_caf, vela_raf};

pub use icons::register as register_icon;
pub use mark_icons::BrowserMarkIcons;

/// What the mark popup reads a mark by (the cluster hands the host only ids).
pub type MarkSource = Rc<dyn Fn(&str) -> Option<TimelineMark>>;

/// Dirty flags of the deferred flush.
pub(crate) const D_LAYOUT: u8 = 1;
pub(crate) const D_ROWS: u8 = 2;
pub(crate) const D_THEME: u8 = 4;

/// The four plot-theme colors the legend, the axis strip and the scroll button wear.
#[derive(Clone, Debug, PartialEq, Default)]
struct PlotTheme {
    background: String,
    text: String,
    border: String,
    up: String,
}

/// The widgets of one chart. Built by the host; dropped (after [`dispose`](Self::dispose))
/// with it.
pub struct ChromeUi {
    me: Weak<ChromeUi>,
    doc: Document,
    plot: Element,
    root: Element,
    platform: Rc<dyn Platform>,
    renderer: RefCell<Weak<NativeRenderer>>,
    translator: RefCell<Option<Rc<dyn Translator>>>,
    mark_source: RefCell<Option<MarkSource>>,
    graveyard: RefCell<Vec<Box<dyn Any>>>,
    purge_armed: Cell<bool>,
    dirty: Cell<u8>,
    raf: RefCell<Option<JsValue>>,
    flush_cb: Closure<dyn FnMut(JsValue)>,
    theme: RefCell<PlotTheme>,
    mobile: Cell<bool>,
    plot_listeners: RefCell<Vec<Listener>>,
    legend: RefCell<legend::Legend>,
    panes: RefCell<panes::PaneControls>,
    axis: RefCell<panes::AxisButtons>,
    scroll: scroll::ScrollButton,
    labels: tooltip::LabelTooltip,
    marks: RefCell<marks::MarkPopover>,
    menu: RefCell<Option<menu::Menu>>,
    mark_icons: RefCell<Option<Rc<BrowserMarkIcons>>>,
    callout_panel: RefCell<Option<legend::OpenCallout>>,
    disposed: Cell<bool>,
}

impl ChromeUi {
    /// Mount the chrome root into `overlay` (the plot's overlay root: absolute over the plot,
    /// above the canvases); `plot` is where the pointer listeners go.
    pub fn new(
        doc: &Document,
        _wrapper: &Element,
        plot: &Element,
        overlay: &Element,
        platform: Rc<dyn Platform>,
    ) -> Rc<Self> {
        style::ensure_styles(doc);
        let root = make(doc, "div", "vela-chrome-ui");
        set_style(&root, "position", "absolute");
        set_style(&root, "inset", "0");
        set_style(&root, "pointer-events", "none");
        let _ = overlay.append_child(&root);
        let ui = Rc::new_cyclic(|me: &Weak<ChromeUi>| {
            let weak = me.clone();
            let flush_cb = Closure::<dyn FnMut(JsValue)>::new(move |_t: JsValue| {
                if let Some(ui) = weak.upgrade() {
                    ui.flush();
                }
            });
            ChromeUi {
                me: me.clone(),
                doc: doc.clone(),
                plot: plot.clone(),
                root: root.clone(),
                platform,
                renderer: RefCell::new(Weak::new()),
                translator: RefCell::new(None),
                mark_source: RefCell::new(None),
                graveyard: RefCell::new(Vec::new()),
                purge_armed: Cell::new(false),
                dirty: Cell::new(0),
                raf: RefCell::new(None),
                flush_cb,
                theme: RefCell::new(PlotTheme::default()),
                mobile: Cell::new(false),
                plot_listeners: RefCell::new(Vec::new()),
                legend: RefCell::new(legend::Legend::new()),
                panes: RefCell::new(panes::PaneControls::new()),
                axis: RefCell::new(panes::AxisButtons::new()),
                scroll: scroll::ScrollButton::new(),
                labels: tooltip::LabelTooltip::new(),
                marks: RefCell::new(marks::MarkPopover::new()),
                menu: RefCell::new(None),
                mark_icons: RefCell::new(None),
                callout_panel: RefCell::new(None),
                disposed: Cell::new(false),
            }
        });
        ui.build_scroll_button();
        ui.build_axis_buttons();
        ui.listen_plot();
        ui
    }

    // ── wiring the owner calls ──

    /// Tell the widgets which renderer they act on. Events that arrive earlier only paint;
    /// the pull queries and actions need the renderer.
    pub fn bind(&self, renderer: &Rc<NativeRenderer>) {
        *self.renderer.borrow_mut() = Rc::downgrade(renderer);
        self.schedule(D_LAYOUT | D_ROWS | D_THEME);
    }

    /// The chart's translator: every string of the chrome resolves through it, and the
    /// rows relabel at once. `None` goes back to English.
    pub fn set_translator(self: &Rc<Self>, translator: Option<Rc<dyn Translator>>) {
        *self.translator.borrow_mut() = translator;
        self.relabel_all();
    }

    /// Where the mark popup reads a mark's title and content by id. Without a source a click
    /// on a glyph opens nothing (the host owns what a mark shows).
    pub fn set_mark_source(&self, source: Option<MarkSource>) {
        *self.mark_source.borrow_mut() = source;
    }

    /// The `indicatorTitles` / `indicatorValues` features (the host's `feature_changed`
    /// forwards them here). Other keys are ignored. Returns whether the key was one of these.
    pub fn feature_changed(&self, key: &str, value: &serde_json::Value) -> bool {
        match key {
            "indicatorTitles" => {
                self.legend_set_titles_visible(value.as_bool().unwrap_or(true));
                true
            }
            "indicatorValues" => {
                self.legend_set_values_visible(value.as_bool().unwrap_or(true));
                true
            }
            _ => false,
        }
    }

    /// X: re-read the plot theme on the next frame.
    pub fn theme_changed(&self) {
        self.schedule(D_THEME);
    }

    /// The pane layout changed (bounds, collapse, order, maximize): reposition on the next
    /// frame (the renderer is borrowed while it calls this).
    pub fn layout_changed(&self) {
        self.schedule(D_LAYOUT);
    }

    /// The host shell's chrome size class.
    pub fn set_layout_mode(self: &Rc<Self>, mode: LayoutMode) {
        let mobile = mode == LayoutMode::Mobile;
        if self.mobile.replace(mobile) == mobile {
            return;
        }
        self.legend_set_mobile(mobile);
        self.panes_set_suspended(mobile);
    }

    /// Remove every widget and free every listener. Idempotent.
    pub fn dispose(&self) {
        if self.disposed.replace(true) {
            return;
        }
        let taken_1 = self.raf.borrow_mut().take();
        if let Some(handle) = taken_1 {
            vela_caf(&handle);
        }
        self.close_menu();
        self.close_callout_panel();
        self.marks_dispose();
        self.legend_dispose();
        self.panes_dispose();
        self.scroll_dispose();
        self.labels.dispose(self);
        self.plot_listeners.borrow_mut().clear();
        self.root.remove();
        self.graveyard.borrow_mut().clear();
    }

    // ── shared services ──

    pub(crate) fn weak(&self) -> Weak<ChromeUi> {
        self.me.clone()
    }

    pub(crate) fn renderer(&self) -> Option<Rc<NativeRenderer>> {
        self.renderer.borrow().upgrade()
    }

    pub(crate) fn is_disposed(&self) -> bool {
        self.disposed.get()
    }

    /// The text of a message in the chart's language.
    pub(crate) fn text(&self, message: &MessageLike) -> String {
        text_of(self.translator.borrow().as_deref(), message, &[])
    }

    pub(crate) fn text_with(&self, message: &MessageLike, values: &[(&str, MessageArg)]) -> String {
        text_of(self.translator.borrow().as_deref(), message, values)
    }

    /// Hand objects over: they are freed from a timer, never from the stack they might
    /// still be running on.
    pub(crate) fn retire(&self, thing: Box<dyn Any>) {
        self.graveyard.borrow_mut().push(thing);
        if self.purge_armed.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                if let Some(ui) = me.upgrade() {
                    ui.purge_armed.set(false);
                    let old = std::mem::take(&mut *ui.graveyard.borrow_mut());
                    drop(old);
                }
            }),
        );
    }

    pub(crate) fn set_timeout(&self, ms: f64, f: impl FnOnce() + 'static) -> TimerId {
        self.platform.set_timeout(ms, Box::new(f))
    }

    pub(crate) fn clear_timeout(&self, id: TimerId) {
        self.platform.clear_timeout(id);
    }

    /// Queue work for the next animation frame (coalesced).
    pub(crate) fn schedule(&self, flags: u8) {
        if self.disposed.get() {
            return;
        }
        self.dirty.set(self.dirty.get() | flags);
        if self.raf.borrow().is_some() {
            return;
        }
        let handle = vela_raf(self.flush_cb.as_ref().unchecked_ref());
        *self.raf.borrow_mut() = Some(handle);
    }

    /// The deferred pass: pull queries are safe here (no renderer borrow is live).
    fn flush(self: &Rc<Self>) {
        self.raf.borrow_mut().take();
        if self.disposed.get() {
            return;
        }
        let dirty = self.dirty.replace(0);
        let renderer = self.renderer();
        if dirty & D_THEME != 0 {
            self.refresh_theme(renderer.as_deref());
        }
        if dirty & D_ROWS != 0 {
            self.legend_flush_rows(renderer.as_deref());
        }
        if dirty & D_LAYOUT != 0
            && let Some(r) = renderer.as_deref()
        {
            let views = r.pane_views();
            let axis_w = r.right_axis_width();
            self.layout_apply(&views, axis_w);
        }
    }

    fn layout_apply(self: &Rc<Self>, views: &[vela_core::render::native::PaneView], axis_w: f64) {
        self.legend_reposition(views, axis_w);
        self.panes_refresh(views, axis_w);
        self.axis_reposition(views, axis_w);
    }

    /// The plot theme the legend, the axis strip and the scroll button wear.
    fn refresh_theme(&self, renderer: Option<&NativeRenderer>) {
        let Some(renderer) = renderer else { return };
        let theme = renderer.plot_theme();
        let pick = |key: &str| {
            theme
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let next = PlotTheme {
            background: pick("background"),
            text: pick("textColor"),
            border: pick("borderColor"),
            up: pick("upColor"),
        };
        if *self.theme.borrow() == next {
            return;
        }
        for (name, value) in [
            ("--vela-lg-bg", &next.background),
            ("--vela-lg-fg", &next.text),
            ("--vela-lg-border", &next.border),
            ("--vela-lg-up", &next.up),
        ] {
            set_style(&self.root, name, value);
        }
        *self.theme.borrow_mut() = next;
    }

    /// A rectangle in the root's (the plot's) coordinates.
    pub(crate) fn rect_in_root(&self, el: &Element) -> (f64, f64, f64, f64) {
        let r = el.get_bounding_client_rect();
        let o = self.root.get_bounding_client_rect();
        (
            r.left() - o.left(),
            r.top() - o.top(),
            r.width(),
            r.height(),
        )
    }

    pub(crate) fn root_size(&self) -> (f64, f64) {
        let o = self.root.get_bounding_client_rect();
        (o.width(), o.height())
    }

    /// Re-read every string in the current language.
    fn relabel_all(self: &Rc<Self>) {
        self.legend_relabel_texts();
        self.panes_relabel();
        self.axis_relabel();
        self.scroll_relabel();
    }

    // ── pointer position on the plot (one dispatcher for every hover widget) ──

    fn listen_plot(self: &Rc<Self>) {
        let mut listeners = Vec::new();
        let weak = self.weak();
        listeners.push(Listener::new(
            self.plot.as_ref(),
            "pointermove",
            false,
            true,
            move |ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                ui.on_plot_move(&ev);
            },
        ));
        let weak = self.weak();
        listeners.push(Listener::new(
            self.plot.as_ref(),
            "pointerleave",
            false,
            true,
            move |_ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                ui.on_plot_leave();
            },
        ));
        let weak = self.weak();
        listeners.push(Listener::new(
            self.plot.as_ref(),
            "pointerdown",
            false,
            true,
            move |_ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                ui.labels.clear(&ui);
            },
        ));
        *self.plot_listeners.borrow_mut() = listeners;
    }

    fn on_plot_move(self: &Rc<Self>, ev: &JsValue) {
        let rect = self.plot.get_bounding_client_rect();
        let (Some(cx), Some(cy)) = (get_f64(ev, "clientX"), get_f64(ev, "clientY")) else {
            return;
        };
        let x = cx - rect.left();
        let y = cy - rect.top();
        let mouse = get_str(ev, "pointerType") == "mouse";
        let Some(renderer) = self.renderer() else {
            return;
        };
        renderer.scroll_proximity_move(x, y);
        if mouse {
            self.labels.moved(self, &renderer, x, y);
        }
        let views = renderer.pane_views();
        let axis_w = renderer.right_axis_width();
        self.panes_hover(&views, axis_w, y);
        if mouse {
            self.axis_hover(&views, axis_w, x, y, rect.width());
        }
    }

    fn on_plot_leave(self: &Rc<Self>) {
        if let Some(renderer) = self.renderer() {
            renderer.scroll_proximity_leave();
        }
        self.labels.clear(self);
        self.panes_leave();
        self.axis_leave();
    }
}
