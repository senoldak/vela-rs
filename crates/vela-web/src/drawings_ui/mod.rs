//! The DOM widgets of the user-drawings layer, driven by
//! [`NativeHost::drawings_ui`](vela_core::render::native::NativeHost::drawings_ui).
//!
//! `vela-core`'s [`UserDrawingController`] owns every decision and reports through ONE host
//! member with a [`DrawingsUiEvent`]; this module owns the elements:
//!
//! | event | widget | status |
//! |---|---|---|
//! | `Toolbar` | the docked left toolbar ([`toolbar`]) | a port of `DrawingToolbar.ts` without the `vela-ui` kit |
//! | `TextEditor` | the inline `<textarea>` ([`editor`]) | a port of the spec in `ui.rs` |
//! | `MagnifierMenu` | a timeframe menu ([`menu`]) | real, small |
//! | `Popup` | the quick bar ([`popup`]) and the full settings panel ([`panel`]) | a port of `DrawingSettingsPopup.ts`, `DrawingSettingsPanel.ts` and `DrawingSettingsSections.ts` |
//!
//! The widgets answer by calling the controller's public methods (`toolbar_*`, `popup_*`,
//! `text_editor_*`, `magnifier_timeframe_picked`) from their own DOM event handlers, never
//! from inside a host call: the controller may be called with the renderer's state
//! borrowed, and a handler that calls back synchronously would re-enter it. The same goes
//! for the controls of the bar and the panel: the model syncing back under an edit reaches
//! them as a `SyncPanel` event from inside the edit's own handler, so they refresh from a
//! timer ([`DrawingsUi::request_sync`]).
//!
//! # No kit
//!
//! The popup and the panel are built on the dialogs' native controls and stylesheet
//! (`crate::dialogs::{controls, css}`), not on the original's `vela-ui` kit. Display text
//! goes through the translator the owner hands over ([`DrawingsUi::set_translator`]); until
//! it does, text reads in English.
//!
//! # Closures and teardown
//!
//! A widget's listeners can be removed from inside their own handler (a flyout item that
//! closes its flyout). A `Closure` must not be freed while it runs, so a removed
//! [`Listener`] is [`retire`](DrawingsUi::retire)d: its element is gone and its handler can
//! no longer be reached, and the closure itself is freed from a timer, on a fresh stack.
//! [`dispose`](DrawingsUi::dispose) frees everything at once.
//!
//! # Colors
//!
//! Everything reads the `--vela-chrome-*` custom properties [`BrowserHost`] writes on the
//! wrapper (`apply_chrome_theme`), with a dark fallback, so a theme switch re-themes the
//! widgets with no code of theirs.

#![allow(clippy::type_complexity)]

mod editor;
mod float;
mod glyphs;
mod kit;
mod menu;
mod model;
mod panel;
mod popup;
mod sections;
mod session;
mod style;
mod toolbar;
mod util;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::i18n::Translator;
use vela_core::render::drawings::{
    DrawingsUiEvent, PopupEvent, PopupRequest, UserDrawingController,
};
use vela_core::render::native::NativeRenderer;
use vela_core::runtime::Platform;
use wasm_bindgen::JsValue;
use web_sys::{Document, Element};

use crate::dialogs::Tr;
use crate::dom::{Listener, get_bool, get_prop, vela_contains};

/// The widgets of one chart. Built by the host; dropped (after
/// [`dispose`](Self::dispose)) with it.
pub struct DrawingsUi {
    me: Weak<DrawingsUi>,
    doc: Document,
    wrapper: Element,
    plot: Element,
    platform: Rc<dyn Platform>,
    renderer: RefCell<Weak<NativeRenderer>>,
    translator: RefCell<Option<Rc<dyn Translator>>>,
    retired: RefCell<Vec<Listener>>,
    purge_armed: Cell<bool>,
    toolbar: RefCell<Option<Rc<toolbar::Toolbar>>>,
    editor: RefCell<Option<editor::Editor>>,
    popup: RefCell<Option<Rc<popup::Popup>>>,
    panel: RefCell<Option<Rc<panel::Panel>>>,
    menu: RefCell<Option<menu::Menu>>,
    /// Bumped whenever the bar or the menu closes: a pending "arm the outside-press
    /// listener" timer of an older generation does nothing.
    generation: Cell<u64>,
    /// A control refresh is waiting for its timer.
    sync_pending: Cell<bool>,
}

impl DrawingsUi {
    /// Widgets that mount into `wrapper` (the toolbar, the bar, the menus) and `plot` (the
    /// inline editor, which sits in plot coordinates).
    pub fn new(
        doc: &Document,
        wrapper: &Element,
        plot: &Element,
        platform: Rc<dyn Platform>,
    ) -> Rc<Self> {
        style::ensure_styles(doc);
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            doc: doc.clone(),
            wrapper: wrapper.clone(),
            plot: plot.clone(),
            platform,
            renderer: RefCell::new(Weak::new()),
            translator: RefCell::new(None),
            retired: RefCell::new(Vec::new()),
            purge_armed: Cell::new(false),
            toolbar: RefCell::new(None),
            editor: RefCell::new(None),
            popup: RefCell::new(None),
            panel: RefCell::new(None),
            menu: RefCell::new(None),
            generation: Cell::new(0),
            sync_pending: Cell::new(false),
        })
    }

    /// Tell the widgets which renderer they act on (its controller answers their
    /// events). Events that arrive before this only paint; nothing needs the controller
    /// until the user does something.
    pub fn bind(&self, renderer: &Rc<NativeRenderer>) {
        *self.renderer.borrow_mut() = Rc::downgrade(renderer);
    }

    /// The chart's translator: the bar, the panel and the menus read in its language. Absent,
    /// they read in English. The owner calls this next to `ChromeUi::set_translator`.
    pub fn set_translator(&self, translator: Option<Rc<dyn Translator>>) {
        *self.translator.borrow_mut() = translator;
        // The bar's fixed labels (Cursor, Measure, ...) are not part of the definition the
        // core re-pushes on a language change: relabel it here.
        let toolbar = self.toolbar.borrow().clone();
        if let Some(toolbar) = toolbar {
            toolbar.relocalize();
        }
    }

    pub(crate) fn tr(&self) -> Tr {
        Tr::new(self.translator.borrow().clone())
    }

    /// The renderer's drawings controller, while both exist.
    pub(crate) fn ctl(&self) -> Option<Rc<UserDrawingController>> {
        self.renderer.borrow().upgrade()?.user_drawings_controller()
    }

    pub(crate) fn renderer(&self) -> Option<Rc<NativeRenderer>> {
        self.renderer.borrow().upgrade()
    }

    /// One event from the controller.
    pub fn handle(&self, event: &DrawingsUiEvent) {
        match event {
            DrawingsUiEvent::Toolbar(state) => self.toolbar_event(state),
            DrawingsUiEvent::TextEditor(e) => self.editor_event(e),
            DrawingsUiEvent::Popup(e) => self.popup_event(e),
            DrawingsUiEvent::MagnifierMenu(request) => self.menu_open(request),
        }
    }

    /// Remove every widget and free every listener. Idempotent.
    pub fn dispose(&self) {
        self.generation.set(self.generation.get() + 1);
        let toolbar = self.toolbar.borrow_mut().take();
        if let Some(toolbar) = toolbar {
            toolbar.dispose();
        }
        let editor = self.editor.borrow_mut().take();
        if let Some(editor) = editor {
            editor.dispose();
        }
        let popup = self.popup.borrow_mut().take();
        if let Some(popup) = popup {
            drop(popup.close());
        }
        let panel = self.panel.borrow_mut().take();
        if let Some(panel) = panel {
            drop(panel.close());
        }
        let menu = self.menu.borrow_mut().take();
        if let Some(menu) = menu {
            menu.dispose();
        }
        self.retired.borrow_mut().clear();
    }

    // ── closures that may be freed from their own handler ──

    /// Hand removed listeners over: they are freed from a timer, never from the stack
    /// they might still be running on.
    pub(crate) fn retire(&self, listeners: Vec<Listener>) {
        if listeners.is_empty() {
            return;
        }
        self.retired.borrow_mut().extend(listeners);
        if self.purge_armed.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                if let Some(ui) = me.upgrade() {
                    ui.purge_armed.set(false);
                    let old = std::mem::take(&mut *ui.retired.borrow_mut());
                    drop(old);
                }
            }),
        );
    }

    /// A timer callback that runs `f` only if the bar / menu generation is still `generation`.
    pub(crate) fn after_tick(&self, generation: u64, f: impl FnOnce(&Rc<DrawingsUi>) + 'static) {
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                if let Some(ui) = me.upgrade()
                    && ui.generation.get() == generation
                {
                    f(&ui);
                }
            }),
        );
    }

    pub(crate) fn bump_generation(&self) -> u64 {
        let next = self.generation.get() + 1;
        self.generation.set(next);
        next
    }

    pub(crate) fn weak(&self) -> Weak<DrawingsUi> {
        self.me.clone()
    }

    /// Run `f` with the controller, for a DOM handler; nothing of the widgets may be
    /// borrowed by the caller (the controller answers by re-entering `handle`).
    pub(crate) fn with_ctl(ui: &Weak<DrawingsUi>, f: impl FnOnce(&UserDrawingController)) {
        if let Some(ctl) = ui.upgrade().and_then(|u| u.ctl()) {
            f(&ctl);
        }
    }

    // ── geometry shared by the floating widgets ──

    /// The plot's top-left corner inside the wrapper, px.
    pub(crate) fn plot_offset(&self) -> (f64, f64) {
        let wrapper = self.wrapper.get_bounding_client_rect();
        let plot = self.plot.get_bounding_client_rect();
        (plot.left() - wrapper.left(), plot.top() - wrapper.top())
    }

    pub(crate) fn wrapper_size(&self) -> (f64, f64) {
        let rect = self.wrapper.get_bounding_client_rect();
        (rect.width(), rect.height())
    }

    /// The chrome surface the host wrote on the wrapper (the callout's fill fallback).
    pub(crate) fn chrome_background(&self) -> String {
        use wasm_bindgen::JsCast;
        self.wrapper
            .unchecked_ref::<web_sys::HtmlElement>()
            .style()
            .get_property_value("--vela-chrome-background")
            .unwrap_or_default()
    }

    // ── the settings popup: the quick bar and the panel ──

    fn popup_event(&self, event: &PopupEvent) {
        match event {
            PopupEvent::Open(request) => self.popup_open(request),
            PopupEvent::Close => self.popup_close(false),
            PopupEvent::SyncPanel => self.request_sync(),
            PopupEvent::OpenPanel => {
                self.open_panel();
            }
        }
    }

    /// Whether `node` (a JS node, possibly null) belongs to the bar, the panel or the
    /// floating shells of either: focus moving there is not a click away.
    pub(crate) fn popup_contains(&self, node: &JsValue) -> bool {
        let popup = self.popup.borrow().clone();
        if popup.is_some_and(|p| p.contains(node)) {
            return true;
        }
        let panel = self.panel.borrow().clone();
        panel.is_some_and(|p| p.contains(node))
    }

    fn panel_is_open(&self) -> bool {
        self.panel.borrow().is_some()
    }

    /// Open the bar for the request: close the one before (and its panel, unless the new bar is
    /// for the drawing the panel edits: a click on it, the end of a drag, keeps the panel).
    fn popup_open(&self, request: &PopupRequest) {
        let keep_panel = self.panel_is_open()
            && request.ids.len() == 1
            && self
                .panel
                .borrow()
                .as_ref()
                .is_some_and(|p| p.core.ids.first() == request.ids.first());
        self.popup_close(keep_panel);
        if request.ids.is_empty() {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        let Some(popup) = popup::Popup::open(&me, request) else {
            return;
        };
        *self.popup.borrow_mut() = Some(popup);
        // Arm the outside-press dismissal one tick later (bubble phase: the chart's own
        // handler has already had the press by then).
        let generation = self.bump_generation();
        self.after_tick(generation, |ui| {
            let popup = ui.popup.borrow().clone();
            if let Some(popup) = popup {
                popup.arm_outside(ui);
            }
        });
    }

    /// Close the bar (and the panel, unless `keep_panel`).
    fn popup_close(&self, keep_panel: bool) {
        let popup = self.popup.borrow_mut().take();
        if let Some(popup) = popup {
            self.bump_generation();
            self.retire(popup.close());
        }
        if !keep_panel {
            self.panel_close();
        }
    }

    /// The bar closes itself: close it, then report the dismissal once.
    pub(crate) fn popup_dismiss(&self, modifier: bool) {
        self.popup_close(false);
        if let Some(ctl) = self.ctl() {
            ctl.popup_dismissed(modifier);
        }
    }

    /// A press anywhere in the document, once the bar has been up a tick.
    pub(crate) fn popup_outside_press(&self, ev: &JsValue) {
        let target = get_prop(ev, "target");
        if self.panel_is_open() {
            if self.popup_contains(&target) {
                return;
            }
            // A press on the chart is the chart's: dragging the drawing keeps the panel open,
            // while picking another drawing or the empty plot ends the edit through the
            // selection.
            if vela_contains(&self.wrapper, &target) {
                return;
            }
            self.panel_close();
            return;
        }
        if self.popup_contains(&target) {
            return;
        }
        let modifier =
            get_bool(ev, "shiftKey") || get_bool(ev, "ctrlKey") || get_bool(ev, "metaKey");
        self.popup_dismiss(modifier);
    }

    /// Open the full settings panel for the drawing the bar is open on. False when there is
    /// none (no bar, or a multi-selection).
    pub(crate) fn open_panel(&self) -> bool {
        let popup = self.popup.borrow().clone();
        let Some(popup) = popup else { return false };
        let Some((id, anchor)) = popup.single() else {
            return false;
        };
        let Some(me) = self.me.upgrade() else {
            return false;
        };
        popup.prepare_for_panel();
        self.panel_close();
        if let Some(panel) = panel::Panel::open(&me, &id, anchor, popup.bar_box()) {
            *self.panel.borrow_mut() = Some(panel);
        }
        true
    }

    /// Close the panel (its edits stay).
    pub(crate) fn panel_close(&self) {
        let panel = self.panel.borrow_mut().take();
        if let Some(panel) = panel {
            self.retire(panel.close());
        }
    }

    /// Refresh the bar's and the panel's controls from the model, from a timer: the model
    /// syncing back reaches us from inside the very handler that made the edit.
    pub(crate) fn request_sync(&self) {
        if self.popup.borrow().is_none() && self.panel.borrow().is_none() {
            return;
        }
        if self.sync_pending.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                let Some(ui) = me.upgrade() else { return };
                ui.sync_pending.set(false);
                let popup = ui.popup.borrow().clone();
                if let Some(popup) = popup {
                    popup.core.refresh();
                    popup.sync();
                }
                let panel = ui.panel.borrow().clone();
                if let Some(panel) = panel {
                    panel.sync();
                }
            }),
        );
    }
}
