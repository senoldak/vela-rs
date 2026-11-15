//! The in-chart dialogs: the chart settings dialog (with the gear that opens it) and the
//! per-indicator settings dialog, driven by the [`NativeHost`](vela_core::render::native::NativeHost)
//! members `has_dialogs`, `open_settings_dialog`, `open_indicator_settings`, `close_dialogs`,
//! `settings_changed` and `feature_changed`.
//!
//! | dialog | original | here |
//! |---|---|---|
//! | chart settings ([`settings`]) | `SettingsDialog.ts` + `makeSettingsButton` of `NativeRenderer.ts` | every tab, row, id and rule; native controls |
//! | indicator settings ([`indicator`]) | `IndicatorInputsDialog.ts` | every input type, tabs, groups, gates; native controls |
//!
//! # No kit
//!
//! The original builds both on the `vela-ui` kit (dialog, tabs, field, select, checkbox,
//! popover, date picker, color field). That kit is not ported: the dialogs here use native
//! elements (`<select>`, `<input type=number|checkbox|color|range|date|time|text>`,
//! `<textarea>`, `<button>`) styled from the `--vela-chrome-*` custom properties
//! [`BrowserHost`](crate::host::BrowserHost) writes on the wrapper, with the kit's structure,
//! sizes, ordering and keyboard behavior reproduced by hand ([`css`], [`controls`]).
//!
//! # Closures and teardown
//!
//! A dialog's listeners live in one bag per open dialog. A bag can be released from inside one
//! of its own handlers (Reset rebuilds the form, a footer button closes it), and a `Closure`
//! must not be freed while it runs: a closed dialog's bag is [`retire`](Dialogs::retire)d and
//! freed from a timer on a fresh stack. [`dispose`](Dialogs::dispose) frees everything at once.
//!
//! # Re-entrancy
//!
//! The renderer calls the host with its state borrowed. Nothing here calls back into it from
//! those members except where the renderer provably holds no borrow (`open_*`, `close_dialogs`,
//! `settings_changed`); the DOM handlers of the controls call it freely.

#![allow(clippy::type_complexity)]

// `color`, `controls` and `css` are shared with the drawing settings popup and panel
// (`crate::drawings_ui`), which build on the same native controls and stylesheet.
pub(crate) mod color;
pub(crate) mod controls;
pub(crate) mod css;
mod icons;
mod indicator;
mod inputs;
mod js;
mod settings;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use serde_json::Value;
use vela_core::chart_types::ChartTypeRegistry;
use vela_core::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use vela_core::model::Inputs;
use vela_core::ports::chart_renderer::ChartRenderer;
use vela_core::ports::market_data::SymbolFacts;
use vela_core::render::native::NativeRenderer;
use vela_core::runtime::{Platform, Unsubscribe};
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element};

use crate::dom::{Listener, attr, create_element, make, on, vela_focus};

/// The current stored input and prop values of an indicator, when the chart keeps them
/// somewhere more current than the renderer's mounted model (the engine's registry).
pub type ValuesSource = Rc<dyn Fn(&str) -> Option<(Inputs, Inputs)>>;
/// The active symbol's style-deciding facts.
pub type FactsSource = Rc<dyn Fn() -> Option<SymbolFacts>>;
/// The id of the app theme in force (the Canvas > Theme row's selection): a registered
/// theme's id, else `dark` or `light`. `None` falls back to the chrome surface's darkness.
pub type ThemeCurrent = Rc<dyn Fn() -> Option<String>>;

/// Display text through the chart's translator (English while none is set).
#[derive(Clone, Default)]
pub struct Tr(Option<Rc<dyn Translator>>);

impl Tr {
    /// Text through `translator` (English when `None`).
    pub fn new(translator: Option<Rc<dyn Translator>>) -> Self {
        Self(translator)
    }

    /// A message resolved in the chart's language.
    pub fn m(&self, message: &MessageLike) -> String {
        text_of(self.0.as_deref(), message, &[])
    }

    /// A message with arguments.
    pub fn mv(&self, message: &MessageLike, args: &[(&str, MessageArg)]) -> String {
        text_of(self.0.as_deref(), message, args)
    }

    /// The translator, English while none is set.
    pub fn dyn_ref(&self) -> &dyn Translator {
        match &self.0 {
            Some(t) => &**t,
            None => vela_core::i18n::english_translator(),
        }
    }

    /// The message `id` (English `en`).
    pub fn t(&self, id: &'static str, en: &'static str) -> String {
        self.m(&msg(id, en))
    }
}

/// What a control builder needs: the document, the listener bag of the open dialog, the
/// translator and an id counter.
#[derive(Clone)]
pub(crate) struct Cx {
    pub doc: Document,
    pub tr: Tr,
    pub bag: Rc<RefCell<Vec<Listener>>>,
    ids: Rc<Cell<u32>>,
}

impl Cx {
    pub(crate) fn new(doc: &Document, tr: Tr) -> Self {
        Self {
            doc: doc.clone(),
            tr,
            bag: Rc::new(RefCell::new(Vec::new())),
            ids: Rc::new(Cell::new(0)),
        }
    }

    /// The same context with a bag of its own (a popup that closes on its own).
    pub fn sub(&self) -> Cx {
        Cx {
            bag: Rc::new(RefCell::new(Vec::new())),
            ..self.clone()
        }
    }

    pub fn el(&self, tag: &str, class: &str) -> Element {
        make(&self.doc, tag, class)
    }

    pub fn text(&self, tag: &str, class: &str, text: &str) -> Element {
        let el = make(&self.doc, tag, class);
        el.set_text_content(Some(text));
        el
    }

    pub fn uid(&self, prefix: &str) -> String {
        let n = self.ids.get() + 1;
        self.ids.set(n);
        format!("{prefix}-{n}")
    }

    pub fn on(&self, el: &Element, event: &'static str, f: impl FnMut(JsValue) + 'static) {
        self.bag.borrow_mut().push(on(el, event, f));
    }
}

/// Add or remove one class of an element.
pub(crate) fn toggle_class(el: &Element, class: &str, on: bool) {
    let current = el.get_attribute("class").unwrap_or_default();
    let has = current.split_whitespace().any(|c| c == class);
    if on == has {
        return;
    }
    let next: Vec<&str> = current
        .split_whitespace()
        .filter(|c| *c != class)
        .chain(on.then_some(class))
        .collect();
    let _ = el.set_attribute("class", &next.join(" "));
}

/// Remove every child of an element.
pub(crate) fn clear_children(el: &Element) {
    while let Some(child) = el.first_child() {
        child.unchecked_ref::<Element>().remove();
    }
}

/// Set or clear a boolean-ish attribute (`data-off`, `hidden`).
pub(crate) fn set_flag_attr(el: &Element, name: &str, on: bool) {
    if on {
        attr(el, name, "");
    } else {
        let _ = el.remove_attribute(name);
    }
}

/// The skeleton of one open dialog: the scrim that catches outside presses, the card, its
/// header (burger, title, close), the body and the footer.
pub(crate) struct Frame {
    pub positioner: Element,
    pub title: Element,
    pub body: Element,
    pub footer: Element,
    pub burger: Element,
}

/// Where a drag of the header started and the card's offset then.
#[derive(Default)]
struct Drag {
    active: Cell<bool>,
    from: Cell<(f64, f64)>,
    offset: Cell<(f64, f64)>,
}

impl Frame {
    /// Build the frame and wire the close button and the header drag. `close` runs on the
    /// close button; the caller wires the scrim press and the keyboard.
    pub fn build(
        cx: &Cx,
        variant: &str,
        center: bool,
        title: &str,
        mobile: bool,
        close: impl Fn() + 'static,
    ) -> Self {
        let positioner = cx.el("div", "vela-dlg-positioner");
        if center {
            attr(&positioner, "data-align", "center");
        }
        let panel = cx.el("div", "vela-dlg-panel");
        let title_id = cx.uid("vela-dlg-title");
        attr(&panel, "role", "dialog");
        attr(&panel, "aria-modal", "false");
        attr(&panel, "aria-labelledby", &title_id);
        attr(&panel, "data-variant", variant);
        panel
            .unchecked_ref::<web_sys::HtmlElement>()
            .set_tab_index(-1);

        let header = cx.el("div", "vela-dlg-header");
        let burger = cx.el("button", "vela-dlg-burger");
        attr(&burger, "type", "button");
        burger.set_inner_html(&icons::icon("burger", 16));
        let title_el = cx.text("h2", "vela-dlg-title", title);
        title_el.set_id(&title_id);
        let closer = cx.el("button", "vela-dlg-close");
        attr(&closer, "type", "button");
        attr(
            &closer,
            "aria-label",
            &cx.tr.t("chart.indicatorSettings.close", "Close"),
        );
        attr(
            &closer,
            "title",
            &cx.tr.t("chart.indicatorSettings.close", "Close"),
        );
        closer.set_inner_html(&icons::icon("close", 16));
        cx.on(&closer, "click", move |_| close());
        let _ = header.append_child(&burger);
        let _ = header.append_child(&title_el);
        let _ = header.append_child(&closer);

        let body = cx.el("div", "vela-dlg-body");
        let footer = cx.el("div", "vela-dlg-footer");
        let _ = panel.append_child(&header);
        let _ = panel.append_child(&body);
        let _ = panel.append_child(&footer);
        let _ = positioner.append_child(&panel);

        // Desktop: the card drags by its header.
        if !mobile {
            let drag = Rc::new(Drag::default());
            {
                let drag = drag.clone();
                let closer = closer.clone();
                let burger = burger.clone();
                cx.on(&header, "pointerdown", move |ev| {
                    if js::dlg_num(&ev, "button") != 0.0 {
                        return;
                    }
                    let target = js::dlg_target(&ev);
                    if js::is_el(&target, &closer) || js::is_el(&target, &burger) {
                        return;
                    }
                    drag.active.set(true);
                    drag.from
                        .set((js::dlg_num(&ev, "clientX"), js::dlg_num(&ev, "clientY")));
                });
            }
            {
                let drag = drag.clone();
                let panel = panel.clone();
                cx.on(&positioner, "pointermove", move |ev| {
                    if !drag.active.get() {
                        return;
                    }
                    let (fx, fy) = drag.from.get();
                    let (ox, oy) = drag.offset.get();
                    let (dx, dy) = (
                        ox + js::dlg_num(&ev, "clientX") - fx,
                        oy + js::dlg_num(&ev, "clientY") - fy,
                    );
                    crate::dom::set_style(
                        &panel,
                        "transform",
                        &format!("translate({dx}px, {dy}px)"),
                    );
                    drag.from
                        .set((js::dlg_num(&ev, "clientX"), js::dlg_num(&ev, "clientY")));
                    drag.offset.set((dx, dy));
                });
            }
            {
                let drag = drag.clone();
                cx.on(&positioner, "pointerup", move |_| drag.active.set(false));
            }
        }
        Self {
            positioner,
            title: title_el,
            body,
            footer,
            burger,
        }
    }
}

/// The in-chart dialogs of one chart. Built by the host; dropped (after
/// [`dispose`](Self::dispose)) with it.
pub struct Dialogs {
    me: Weak<Dialogs>,
    doc: Document,
    container: Element,
    wrapper: Element,
    plot: Element,
    /// Where the chart dialogs mount instead of the wrapper (the original's `dialogHost`: a
    /// widget puts its root here, so a small cell never clips a dialog).
    dialog_host: RefCell<Option<Element>>,
    platform: Rc<dyn Platform>,
    renderer: RefCell<Weak<NativeRenderer>>,
    registry: RefCell<Option<Rc<RefCell<ChartTypeRegistry>>>>,
    translator: RefCell<Option<Rc<dyn Translator>>>,
    values_source: RefCell<Option<ValuesSource>>,
    facts_source: RefCell<Option<FactsSource>>,
    theme_choices: RefCell<Option<Vec<(String, String)>>>,
    theme_current: RefCell<Option<ThemeCurrent>>,
    on_backdrop_close: RefCell<Option<Rc<dyn Fn()>>>,
    gear: RefCell<Option<Element>>,
    gear_click: RefCell<Option<Listener>>,
    settings: RefCell<Option<Rc<settings::SettingsDialog>>>,
    indicator: RefCell<Option<Rc<indicator::IndicatorDialog>>>,
    retired: RefCell<Vec<Box<dyn Any>>>,
    purge_armed: Cell<bool>,
    config_sub: RefCell<Option<Unsubscribe>>,
    disposed: Cell<bool>,
}

impl Dialogs {
    /// The dialogs of a chart whose wrapper is `wrapper` (the dialogs mount there) and whose
    /// plot is `plot` (the gear sits in it, above the time axis).
    pub fn new(
        doc: &Document,
        container: &Element,
        wrapper: &Element,
        plot: &Element,
        platform: Rc<dyn Platform>,
    ) -> Rc<Self> {
        css::ensure_styles(doc);
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            doc: doc.clone(),
            container: container.clone(),
            wrapper: wrapper.clone(),
            plot: plot.clone(),
            dialog_host: RefCell::new(None),
            platform,
            renderer: RefCell::new(Weak::new()),
            registry: RefCell::new(None),
            translator: RefCell::new(None),
            values_source: RefCell::new(None),
            facts_source: RefCell::new(None),
            theme_choices: RefCell::new(None),
            theme_current: RefCell::new(None),
            on_backdrop_close: RefCell::new(None),
            gear: RefCell::new(None),
            gear_click: RefCell::new(None),
            settings: RefCell::new(None),
            indicator: RefCell::new(None),
            retired: RefCell::new(Vec::new()),
            purge_armed: Cell::new(false),
            config_sub: RefCell::new(None),
            disposed: Cell::new(false),
        })
    }

    /// Tell the dialogs which renderer they act on and which chart-type registry it reads
    /// (the chart-type tabs come from it). Events that arrive earlier only record state.
    pub fn bind(&self, renderer: &Rc<NativeRenderer>, registry: Rc<RefCell<ChartTypeRegistry>>) {
        *self.renderer.borrow_mut() = Rc::downgrade(renderer);
        *self.registry.borrow_mut() = Some(registry);
        // An applied config (the JS API, a persisted restore) while the dialog is open: the
        // controls re-read it. The dialog's own edits already show.
        let me = self.me.clone();
        let unsub = renderer.on_config_changed(Box::new(move || {
            if let Some(d) = me.upgrade() {
                d.config_changed();
            }
        }));
        *self.config_sub.borrow_mut() = unsub;
    }

    /// Mount the settings and indicator dialogs in `host` (an element that spans the widget)
    /// instead of the chart's own wrapper. `None` goes back to the wrapper.
    pub fn set_dialog_host(&self, host: Option<Element>) {
        *self.dialog_host.borrow_mut() = host;
    }

    /// Put a dialog frame on the page: in the dialog host when there is one (carrying the
    /// chrome colors the wrapper holds, which the host does not), else in the wrapper.
    pub(crate) fn mount_frame(&self, positioner: &Element) {
        let host = self.dialog_host.borrow().clone();
        match host {
            Some(host) => {
                let from = self.wrapper.unchecked_ref::<web_sys::HtmlElement>().style();
                let to = positioner.unchecked_ref::<web_sys::HtmlElement>().style();
                for key in ["background", "text-color", "border-color", "font-family"] {
                    let name = format!("--vela-chrome-{key}");
                    if let Ok(v) = from.get_property_value(&name)
                        && !v.is_empty()
                    {
                        let _ = to.set_property(&name, &v);
                    }
                }
                let _ = host.append_child(positioner);
            }
            None => {
                let _ = self.wrapper.append_child(positioner);
            }
        }
    }

    /// The chart's translator; absent, the dialogs read in English.
    pub fn set_translator(&self, translator: Option<Rc<dyn Translator>>) {
        *self.translator.borrow_mut() = translator;
        // An open settings dialog rebuilds in the new language, on the same tab.
        self.defer(|d| d.refresh_settings());
    }

    /// Where the indicator dialog reads an indicator's current values (default: the
    /// renderer's mounted model).
    pub fn set_values_source(&self, source: Option<ValuesSource>) {
        *self.values_source.borrow_mut() = source;
    }

    /// Where the Type row reads the active symbol's facts (which styles to grey out).
    pub fn set_facts_source(&self, source: Option<FactsSource>) {
        *self.facts_source.borrow_mut() = source;
    }

    /// The Canvas > Theme row's choices as `(id, label)`, overriding the default list (the
    /// translated `dark` / `light`, then every theme added with `registerTheme`, read each
    /// time the dialog builds).
    pub fn set_theme_choices(&self, choices: Option<Vec<(String, String)>>) {
        *self.theme_choices.borrow_mut() = choices;
    }

    /// Where the Canvas > Theme row reads the app theme in force (the original reads the
    /// registered theme the app theme is, else the surface's darkness).
    pub fn set_theme_current(&self, source: Option<ThemeCurrent>) {
        *self.theme_current.borrow_mut() = source;
    }

    /// Runs when a press on an indicator dialog's scrim closes it (the legend drops its
    /// selection, as the original's `onBackdropClose` does).
    pub fn set_on_backdrop_close(&self, hook: Option<Rc<dyn Fn()>>) {
        *self.on_backdrop_close.borrow_mut() = hook;
    }

    pub(crate) fn backdrop_closed(&self) {
        let hook = self.on_backdrop_close.borrow().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(crate) fn tr(&self) -> Tr {
        Tr(self.translator.borrow().clone())
    }

    pub(crate) fn renderer(&self) -> Option<Rc<NativeRenderer>> {
        self.renderer.borrow().upgrade()
    }

    /// The stable chrome background the host wrote on the wrapper (dark or light surface).
    pub(crate) fn chrome_background(&self) -> String {
        self.wrapper
            .unchecked_ref::<web_sys::HtmlElement>()
            .style()
            .get_property_value("--vela-chrome-background")
            .unwrap_or_default()
    }

    fn mobile(&self) -> bool {
        self.container.get_attribute("data-vela-layout").as_deref() == Some("mobile")
    }

    // ── the host members ──

    /// `NativeHost::has_dialogs`.
    pub fn available(&self) -> bool {
        !self.disposed.get()
    }

    /// `NativeHost::feature_changed`: the `settings` feature shows or hides the gear (and
    /// closes the dialog on the way out). The other widget features are the legend's.
    pub fn feature_changed(&self, key: &str, value: &Value) {
        if key != "settings" {
            return;
        }
        let on = match value {
            Value::Null | Value::Bool(false) => false,
            Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
            Value::String(s) => !s.is_empty(),
            _ => true,
        };
        self.set_gear(on);
    }

    /// `NativeHost::settings_changed`: the host sections or the visibility policy changed.
    /// An open dialog rebuilds in place on the same tab (deferred: the renderer may still be
    /// inside the call that changed them).
    pub fn settings_changed(&self) {
        self.defer(|d| d.refresh_settings());
    }

    /// `NativeHost::open_settings_dialog`: toggle the chart settings dialog, landing on
    /// `section` (a tab title). A section asked for while the dialog is open switches tabs
    /// instead of closing it.
    pub fn open_settings(&self, section: Option<&str>) {
        if self.disposed.get() {
            return;
        }
        let dialog = self.settings_dialog();
        if let Some(section) = section
            && dialog.is_open()
        {
            dialog.show_section(section);
            return;
        }
        if dialog.is_open() {
            dialog.close();
        } else {
            dialog.open(section);
        }
        self.sync_gear_state();
    }

    /// `NativeHost::open_indicator_settings`: open one indicator's dialog (a silent no-op
    /// for an unknown id or an indicator without inputs).
    pub fn open_indicator(&self, id: &str) {
        if self.disposed.get() {
            return;
        }
        self.indicator_dialog().open_for(id);
    }

    /// `NativeHost::close_dialogs`: close every dialog that is open.
    pub fn close_all(&self) {
        let indicator = self.indicator.borrow().clone();
        if let Some(i) = indicator {
            i.close();
        }
        let settings = self.settings.borrow().clone();
        if let Some(s) = settings {
            s.close();
        }
        self.sync_gear_state();
    }

    /// A legend row went away: its open dialog goes with it (`InputsUI.remove`).
    pub fn indicator_removed(&self, id: &str) {
        let dialog = self.indicator.borrow().clone();
        if let Some(d) = dialog
            && d.open_id().as_deref() == Some(id)
        {
            d.close();
        }
    }

    /// The layout mode flipped: an open indicator dialog is re-presented in the new mode
    /// (`InputsUI.setLayoutMode`: fullscreen on mobile).
    pub fn layout_changed(&self) {
        let dialog = self.indicator.borrow().clone();
        if let Some(d) = dialog
            && let Some(id) = d.open_id()
        {
            d.close();
            self.open_indicator(&id);
        }
    }

    /// Whether the chart settings dialog is open.
    pub fn settings_open(&self) -> bool {
        self.settings.borrow().as_ref().is_some_and(|s| s.is_open())
    }

    /// Whether an indicator dialog is open, and for which indicator.
    pub fn indicator_open(&self) -> Option<String> {
        self.indicator.borrow().as_ref().and_then(|i| i.open_id())
    }

    /// Re-seed an open chart settings dialog from the renderer's config (after an outside
    /// change such as a theme swap).
    pub fn refresh_open(&self) {
        self.defer(|d| d.refresh_settings());
    }

    /// Remove everything the dialogs put in the page and free every listener. Idempotent.
    pub fn dispose(&self) {
        if self.disposed.replace(true) {
            return;
        }
        let unsub = self.config_sub.borrow_mut().take();
        if let Some(unsub) = unsub {
            unsub();
        }
        let settings = self.settings.borrow_mut().take();
        if let Some(s) = settings {
            s.destroy();
        }
        let indicator = self.indicator.borrow_mut().take();
        if let Some(i) = indicator {
            i.destroy();
        }
        self.gear_click.borrow_mut().take();
        let taken_1 = self.gear.borrow_mut().take();
        if let Some(gear) = taken_1 {
            gear.remove();
        }
        self.retired.borrow_mut().clear();
    }

    // ── internals ──

    fn settings_dialog(&self) -> Rc<settings::SettingsDialog> {
        let mut slot = self.settings.borrow_mut();
        slot.get_or_insert_with(|| settings::SettingsDialog::new(self.me.clone()))
            .clone()
    }

    fn indicator_dialog(&self) -> Rc<indicator::IndicatorDialog> {
        let mut slot = self.indicator.borrow_mut();
        slot.get_or_insert_with(|| indicator::IndicatorDialog::new(self.me.clone()))
            .clone()
    }

    fn refresh_settings(&self) {
        let dialog = self.settings.borrow().clone();
        if let Some(dialog) = dialog
            && dialog.is_open()
        {
            dialog.refresh();
        }
    }

    fn config_changed(&self) {
        let dialog = self.settings.borrow().clone();
        if let Some(dialog) = dialog
            && dialog.is_open()
            && !dialog.editing()
        {
            self.defer(|d| d.refresh_settings());
        }
    }

    /// Run `f` from a timer, with the dialogs, if they are still alive.
    fn defer(&self, f: impl FnOnce(&Rc<Dialogs>) + 'static) {
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                if let Some(d) = me.upgrade()
                    && !d.disposed.get()
                {
                    f(&d);
                }
            }),
        );
    }

    /// Hand a closed dialog's listeners over: they are freed from a timer, never from the
    /// stack of a handler that may still be running.
    pub(crate) fn retire(&self, bag: Rc<RefCell<Vec<Listener>>>) {
        let listeners = std::mem::take(&mut *bag.borrow_mut());
        if listeners.is_empty() {
            return;
        }
        self.retired.borrow_mut().push(Box::new(listeners));
        if self.purge_armed.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.platform.set_timeout(
            0.0,
            Box::new(move || {
                if let Some(d) = me.upgrade() {
                    d.purge_armed.set(false);
                    let old = std::mem::take(&mut *d.retired.borrow_mut());
                    drop(old);
                }
            }),
        );
    }

    /// Where focus goes when a dialog closes: the element that had it at open, else the
    /// chart's data canvas.
    pub(crate) fn restore_focus(&self, prev: Option<Element>) {
        if let Some(prev) = prev
            && js::dlg_attached(&prev)
        {
            vela_focus(&prev);
            return;
        }
        if let Some(r) = self.renderer() {
            r.focus();
        }
    }

    /// The element that has focus now.
    pub(crate) fn active_element(&self) -> Option<Element> {
        let active = js::dlg_active(&self.doc);
        if active.is_null() || active.is_undefined() {
            None
        } else {
            Some(active.unchecked_into())
        }
    }

    // ── the gear ──

    fn set_gear(&self, enabled: bool) {
        if enabled {
            if self.gear.borrow().is_none() {
                let gear = create_element(&self.doc, "button");
                gear.set_class_name("vela-gear");
                attr(&gear, "type", "button");
                attr(&gear, "aria-haspopup", "dialog");
                attr(&gear, "aria-expanded", "false");
                gear.set_inner_html(&icons::icon("gear", 14));
                let label = self
                    .tr()
                    .t("chart.settings.chartSettings", "Chart settings");
                attr(&gear, "aria-label", &label);
                attr(&gear, "title", &label);
                let me = self.me.clone();
                let click = on(&gear, "click", move |_| {
                    if let Some(d) = me.upgrade() {
                        d.open_settings(None);
                    }
                });
                let _ = self.plot.append_child(&gear);
                *self.gear.borrow_mut() = Some(gear);
                *self.gear_click.borrow_mut() = Some(click);
            }
            if let Some(gear) = self.gear.borrow().as_ref() {
                crate::dom::set_style(gear, "display", "flex");
            }
        } else {
            if let Some(gear) = self.gear.borrow().as_ref() {
                crate::dom::set_style(gear, "display", "none");
            }
            let settings = self.settings.borrow().clone();
            if let Some(s) = settings {
                s.close();
            }
        }
    }

    pub(crate) fn sync_gear_state(&self) {
        if let Some(gear) = self.gear.borrow().as_ref() {
            attr(
                gear,
                "aria-expanded",
                if self.settings_open() {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }
}
