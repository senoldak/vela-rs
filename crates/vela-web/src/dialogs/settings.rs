//! The chart settings dialog: a port of `SettingsDialog.ts`.
//!
//! Tabs (rail on the left, one pane each), in the original's order: Symbol (chart type, the
//! per-style cosmetics, animation, time zone), host sections placed in or after it, the
//! chart-type SDK tabs placed after it, Scales and lines (price scale, crosshair), Canvas
//! (background and text, grid, margins, theme), Events (mark groups, present while marks
//! name groups), the remaining chart-type tabs, the remaining host sections. Every control
//! edits the renderer through a minimal nested patch to `apply_config`; the factory reset
//! applies `factory_reset_document`; the visibility policy drops ids at build time.
//!
//! Differences from the original are listed in the report: native controls for the kit's
//! (a swatch + opacity slider for the color popover, a select for the line-width glyph
//! menu), host-section rows are data (no callbacks cross the seam: edits raise a DOM event),
//! and an open dialog does not rebuild on a language change (it reads the translator at
//! build time).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use serde_json::{Map, Value, json};
use vela_core::chart_types::{
    ChartTypeRegistry, ChartTypeSettingsInstance, ChartTypeSettingsSection, SettingsInlineControl,
    SettingsLayout, SettingsPlacement, SettingsRowDescriptor, SettingsRowWhen,
    SettingsSelectOption, SettingsValue, SettingsValueType, SettingsVisibility,
    normalize_settings_row, price_style_applies, price_style_message, price_style_unavailable,
    settings_row_value_keys, settings_row_visible,
};
use vela_core::i18n::{MessageLike, msg, source_text};
use vela_core::marks::{MarkGroup, mark_group_own_visible, mark_group_rows, mark_group_visible};
use vela_core::ports::chart_renderer::ChartRenderer;
use vela_core::render::chrome::settings_visibility::{
    HostRowId, HostRowIdSource, HostSectionIdSource, MARKS_GROUPS_SETTINGS_ID, MARKS_SETTINGS_ID,
    filter_hidden_host_rows, filter_hidden_rows, host_section_id, mark_group_settings_id,
    settings_id_hidden, settings_id_slug,
};
use vela_core::render::core::chart_config::{has_own_candle_paint, price_style_ids};
use vela_core::render::native::NativeRenderer;
use vela_core::timezones::{normalize_timezone, timezones, tz_menu_label};
use wasm_bindgen::JsCast;
use web_sys::Element;

use super::controls::{self, Control, NumOpts, SelOpt};
use super::js::*;
use super::{Cx, Dialogs, Frame, Tr, clear_children, set_flag_attr, toggle_class};
use crate::dom::{Listener, attr, set_style};

/// Built-in styles drawn with another style's settings.
fn shared_group(style: &str) -> &str {
    match style {
        "heikinashi" => "candles",
        "linemarkers" | "stepline" => "line",
        other => other,
    }
}

// ── config access ──

fn at<'a>(c: &'a Value, path: &[&str]) -> &'a Value {
    let mut v = c;
    for p in path {
        v = v.get(*p).unwrap_or(&Value::Null);
    }
    v
}
fn jb(c: &Value, p: &[&str]) -> bool {
    at(c, p).as_bool().unwrap_or(false)
}
fn js(c: &Value, p: &[&str]) -> String {
    at(c, p).as_str().unwrap_or_default().to_string()
}
fn jn(c: &Value, p: &[&str]) -> f64 {
    at(c, p).as_f64().unwrap_or(0.0)
}

/// `{a: {b: value}}` from `["a", "b"]`.
fn nest(path: &[&str], value: Value) -> Value {
    path.iter()
        .rev()
        .fold(value, |inner, key| json!({ *key: inner }))
}

/// A value two closures share: the source of a control and where its edits land.
fn held<T: Clone + 'static>(init: T) -> (Rc<dyn Fn() -> T>, Rc<dyn Fn(T)>) {
    let cell = Rc::new(RefCell::new(init));
    let c2 = cell.clone();
    (
        Rc::new(move || cell.borrow().clone()),
        Rc::new(move |v| *c2.borrow_mut() = v),
    )
}

/// Sends patches to the renderer, marking them as the dialog's own.
struct Emit {
    dlg: Weak<SettingsDialog>,
    renderer: Rc<NativeRenderer>,
}

impl Emit {
    fn send(&self, patch: Value) {
        let dlg = self.dlg.upgrade();
        if let Some(d) = &dlg {
            d.editing.set(true);
        }
        self.renderer.apply_config(&patch);
        if let Some(d) = &dlg {
            d.editing.set(false);
        }
    }
    fn path(&self, path: &[&str], value: Value) {
        self.send(nest(path, value));
    }
    fn typed(&self, type_id: &str, key: &str, value: Value) {
        self.send(json!({ "chartTypes": { type_id: { key: value } } }));
    }
}

// ── row builders ──

fn title_el(cx: &Cx, text: &str) -> Element {
    let el = cx.text("div", "vela-sd-title", text);
    attr(&el, "role", "heading");
    attr(&el, "aria-level", "3");
    el
}

fn group_el(cx: &Cx) -> Element {
    cx.el("div", "vela-sd-grp")
}

fn row_with(cx: &Cx, label: &str, for_id: Option<&str>, controls: Vec<Element>) -> Element {
    let row = cx.el("div", "vela-sd-row");
    let l = cx.text("label", "vela-sd-label", label);
    if let Some(id) = for_id {
        attr(&l, "for", id);
    }
    let cell = cx.el("div", "vela-sd-cell");
    for c in controls {
        let _ = cell.append_child(&c);
    }
    let _ = row.append_child(&l);
    let _ = row.append_child(&cell);
    row
}

/// A toggle row: the checkbox sits left of its label; dependent controls ride the control
/// column and dim while it is off. The returned function re-reads the toggle.
fn toggle_row(
    cx: &Cx,
    label: &str,
    get: Rc<dyn Fn() -> bool>,
    on: Rc<dyn Fn(bool)>,
    controls: Vec<Element>,
    info: Option<Element>,
) -> (Element, Rc<dyn Fn()>) {
    let id = cx.uid("vela-sd");
    let cell = cx.el("div", "vela-sd-cell");
    let has_controls = !controls.is_empty();
    for c in controls {
        let _ = cell.append_child(&c);
    }
    let cell2 = cell.clone();
    let get2 = get.clone();
    let check = controls::checkbox(
        cx,
        &id,
        "",
        get.clone(),
        Rc::new(move |v| {
            set_flag_attr(&cell2, "data-off", !v);
            on(v);
        }),
    );
    set_flag_attr(&cell, "data-off", !get2());
    let l = cx.text("span", "", label);
    let wrap = cx.el("label", "vela-sd-tog");
    attr(&wrap, "for", &id);
    let _ = wrap.append_child(&check.el);
    let _ = wrap.append_child(&l);
    let sync = {
        let (cell, sync, get) = (cell.clone(), check.sync.clone(), get2.clone());
        Rc::new(move || {
            sync();
            set_flag_attr(&cell, "data-off", !get());
        })
    };
    if !has_controls {
        let row = cx.el("div", "vela-sd-bool");
        let _ = row.append_child(&wrap);
        if let Some(i) = info {
            let _ = row.append_child(&i);
        }
        return (row, sync);
    }
    let row = cx.el("div", "vela-sd-row");
    let _ = row.append_child(&wrap);
    let _ = row.append_child(&cell);
    (row, sync)
}

fn swatch(cx: &Cx, aria: &str, value: &str, on: Rc<dyn Fn(String)>) -> Control {
    let (get, set) = held(value.to_string());
    controls::color_field(
        cx,
        &cx.uid("vela-sd"),
        aria,
        false,
        get,
        Rc::new(move |v| {
            set(v.clone());
            on(v);
        }),
    )
}

fn color_row(cx: &Cx, label: &str, value: &str, on: Rc<dyn Fn(String)>) -> Element {
    let id = cx.uid("vela-sd");
    let (get, set) = held(value.to_string());
    let c = controls::color_field(
        cx,
        &id,
        label,
        false,
        get,
        Rc::new(move |v| {
            set(v.clone());
            on(v);
        }),
    );
    row_with(cx, label, Some(&id), vec![c.el])
}

#[allow(clippy::too_many_arguments)]
fn number_row(
    cx: &Cx,
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    on: Rc<dyn Fn(f64)>,
    unit: Option<MessageLike>,
) -> Element {
    let id = cx.uid("vela-sd");
    let unit_el = unit.as_ref().map(|_| cx.el("span", "vela-sd-unit"));
    let show = {
        let (u, unit, tr) = (unit_el.clone(), unit.clone(), cx.tr.clone());
        Rc::new(move |count: f64| {
            if let (Some(u), Some(m)) = (&u, &unit) {
                u.set_text_content(Some(&tr.mv(m, &[("count", count.into())])));
            }
        })
    };
    show(value);
    let (get, set) = held(value);
    let n = controls::number(
        cx,
        &id,
        NumOpts {
            min: Some(min),
            max: Some(max),
            step: Some(step),
            live: true,
            aria: label.to_string(),
            ..NumOpts::default()
        },
        get,
        Rc::new(move |v| {
            set(v);
            show(v);
            on(v);
        }),
    );
    let mut cs = vec![n.el];
    cs.extend(unit_el);
    row_with(cx, label, Some(&id), cs)
}

fn select_row(
    cx: &Cx,
    label: &str,
    value: &str,
    opts: &[SelOpt],
    on: Rc<dyn Fn(String)>,
) -> Element {
    let id = cx.uid("vela-sd");
    let (get, set) = held(value.to_string());
    let s = controls::select(
        cx,
        &id,
        label,
        opts,
        get,
        Rc::new(move |v| {
            set(v.clone());
            on(v);
        }),
    );
    row_with(cx, label, Some(&id), vec![s.el])
}

fn when_visible(when: &Option<SettingsRowWhen>, bag: &Rc<RefCell<Map<String, Value>>>) -> bool {
    settings_row_visible(when.as_ref(), &bag.borrow())
}

// ── host rows ──

#[derive(Clone)]
struct HRow {
    src: HostRowIdSource,
    json: Value,
}

impl HostRowId for HRow {
    fn kind(&self) -> &str {
        &self.src.kind
    }
    fn label(&self) -> &MessageLike {
        &self.src.label
    }
    fn explicit_id(&self) -> Option<&str> {
        self.src.id.as_deref()
    }
}

// ── the dialog ──

struct TabH {
    title: String,
    label: String,
    pane: Element,
    button: Element,
    style: Option<String>,
    always: bool,
    sub: bool,
}

/// The built shell: its tabs and what switching one touches.
struct Shell {
    tabs: RefCell<Vec<TabH>>,
    title: Element,
    default_title: String,
    rail: Element,
    scrim: Element,
    mobile: bool,
    dlg: Weak<SettingsDialog>,
}

impl Shell {
    fn activate(&self, idx: usize) {
        let tabs = self.tabs.borrow();
        for (i, t) in tabs.iter().enumerate() {
            set_style(&t.pane, "display", if i == idx { "block" } else { "none" });
            set_flag_attr(&t.pane, "data-shown", i == idx);
            attr(
                &t.button,
                "aria-selected",
                if i == idx { "true" } else { "false" },
            );
            t.button
                .unchecked_ref::<web_sys::HtmlElement>()
                .set_tab_index(if i == idx { 0 } else { -1 });
        }
        if let (Some(t), Some(d)) = (tabs.get(idx), self.dlg.upgrade()) {
            *d.active_section.borrow_mut() = Some(t.title.clone());
        }
        if self.mobile {
            let label = tabs
                .get(idx)
                .map_or(self.default_title.clone(), |t| t.label.clone());
            self.title.set_text_content(Some(&label));
            drop(tabs);
            self.toggle_rail(Some(false));
        }
    }

    fn toggle_rail(&self, open: Option<bool>) {
        let on = open.unwrap_or_else(|| !self.rail.has_attribute("data-open"));
        set_flag_attr(&self.rail, "data-open", on);
        set_flag_attr(&self.scrim, "data-open", on);
    }

    fn sync_type_tabs(&self, active: &str) {
        let mut hid_active = false;
        for t in self.tabs.borrow().iter() {
            let Some(style) = &t.style else { continue };
            let show = t.always || style == active;
            set_style(&t.button, "display", if show { "" } else { "none" });
            if !show && t.pane.has_attribute("data-shown") {
                hid_active = true;
            }
        }
        if hid_active {
            self.activate(0);
        }
    }

    fn shown_index(&self) -> Option<usize> {
        self.tabs
            .borrow()
            .iter()
            .position(|t| t.button.get_attribute("aria-selected").as_deref() == Some("true"))
    }
}

struct Session {
    cx: Cx,
    frame: Frame,
    shell: RefCell<Option<Rc<Shell>>>,
    prev_focus: Option<Element>,
    mobile: bool,
}

/// The chart settings dialog of one chart.
pub struct SettingsDialog {
    d: Weak<Dialogs>,
    me: RefCell<Weak<SettingsDialog>>,
    session: RefCell<Option<Rc<Session>>>,
    active_section: RefCell<Option<String>>,
    inst_active: RefCell<HashMap<String, usize>>,
    group_active: RefCell<HashMap<String, String>>,
    mark_choices: RefCell<HashMap<String, bool>>,
    editing: Cell<bool>,
}

impl SettingsDialog {
    pub fn new(d: Weak<Dialogs>) -> Rc<Self> {
        let s = Rc::new(Self {
            d,
            me: RefCell::new(Weak::new()),
            session: RefCell::new(None),
            active_section: RefCell::new(None),
            inst_active: RefCell::new(HashMap::new()),
            group_active: RefCell::new(HashMap::new()),
            mark_choices: RefCell::new(HashMap::new()),
            editing: Cell::new(false),
        });
        *s.me.borrow_mut() = Rc::downgrade(&s);
        s
    }

    pub fn is_open(&self) -> bool {
        self.session.borrow().is_some()
    }

    /// Whether the renderer is applying one of this dialog's own edits right now.
    pub fn editing(&self) -> bool {
        self.editing.get()
    }

    /// Switch an open dialog to a tab by title (no-op when closed or unknown).
    pub fn show_section(&self, section: &str) {
        let shell = self
            .session
            .borrow()
            .as_ref()
            .and_then(|s| s.shell.borrow().clone());
        if let Some(shell) = shell {
            let at = shell
                .tabs
                .borrow()
                .iter()
                .position(|t| t.title.eq_ignore_ascii_case(section));
            if let Some(at) = at {
                shell.activate(at);
            }
        }
    }

    pub fn open(self: &Rc<Self>, section: Option<&str>) {
        self.close_inner(false);
        let Some(d) = self.d.upgrade() else { return };
        let Some(renderer) = d.renderer() else { return };
        let tr = d.tr();
        let cx = Cx::new(&d.doc, tr.clone());
        let mobile = d.mobile();
        let weak = self.me.borrow().clone();
        let frame = Frame::build(
            &cx,
            "settings",
            false,
            &tr.t("chart.settings.chartSettings", "Chart settings"),
            mobile,
            {
                let w = weak.clone();
                move || {
                    if let Some(s) = w.upgrade() {
                        s.close();
                    }
                }
            },
        );
        // Footer: Reset defaults.
        {
            let w = weak.clone();
            let b = controls::button(
                &cx,
                &tr.t("chart.settings.resetDefaults", "Reset defaults"),
                false,
                false,
                move || {
                    if let Some(s) = w.upgrade() {
                        s.reset();
                    }
                },
            );
            let _ = frame.footer.append_child(&b);
        }
        // Outside press closes; Escape closes.
        {
            let w = weak.clone();
            let pos = frame.positioner.clone();
            cx.on(&frame.positioner, "pointerdown", move |ev| {
                if is_el(&dlg_target(&ev), &pos)
                    && let Some(s) = w.upgrade()
                {
                    s.close();
                }
            });
        }
        {
            let w = weak.clone();
            let l = Listener::new(&d.doc, "keydown", false, false, move |ev| {
                if dlg_key(&ev) == "Escape"
                    && let Some(s) = w.upgrade()
                    && s.is_open()
                {
                    crate::dom::prevent_default(&ev);
                    s.close();
                }
            });
            cx.bag.borrow_mut().push(l);
        }
        d.mount_frame(&frame.positioner);
        let session = Rc::new(Session {
            cx,
            frame,
            shell: RefCell::new(None),
            prev_focus: d.active_element(),
            mobile,
        });
        *self.session.borrow_mut() = Some(session.clone());
        let config = renderer.get_config().unwrap_or(Value::Null);
        self.build_content(&session, &renderer, &config, section.map(str::to_string));
        // Burger (mobile).
        {
            let w = weak.clone();
            cx_burger(&session, move || {
                if let Some(s) = w.upgrade()
                    && let Some(sh) = s
                        .session
                        .borrow()
                        .as_ref()
                        .and_then(|x| x.shell.borrow().clone())
                {
                    sh.toggle_rail(None);
                }
            });
        }
        // Focus the active tab.
        if let Some(sh) = session.shell.borrow().as_ref()
            && let Some(i) = sh.shown_index()
            && let Some(t) = sh.tabs.borrow().get(i)
        {
            crate::dom::vela_focus(&t.button);
        }
        d.sync_gear_state();
    }

    /// Re-seed every control from the renderer's config, in place, on the same tab.
    pub fn refresh(self: &Rc<Self>) {
        let session = self.session.borrow().clone();
        let Some(session) = session else { return };
        let Some(d) = self.d.upgrade() else { return };
        let Some(renderer) = d.renderer() else { return };
        clear_children(&session.frame.body);
        let config = renderer.get_config().unwrap_or(Value::Null);
        let section = self.active_section.borrow().clone();
        self.build_content(&session, &renderer, &config, section);
    }

    fn reset(self: &Rc<Self>) {
        let Some(d) = self.d.upgrade() else { return };
        let Some(renderer) = d.renderer() else { return };
        if let Some(doc) = renderer.factory_reset_document() {
            self.editing.set(true);
            renderer.apply_config(&doc);
            self.editing.set(false);
        }
        self.refresh();
    }

    pub fn close(&self) {
        self.close_inner(true);
    }

    fn close_inner(&self, restore: bool) {
        let session = self.session.borrow_mut().take();
        let Some(session) = session else { return };
        session.frame.positioner.remove();
        if let Some(d) = self.d.upgrade() {
            d.retire(session.cx.bag.clone());
            d.sync_gear_state();
            if restore {
                d.restore_focus(session.prev_focus.clone());
            }
        }
    }

    pub fn destroy(&self) {
        self.close_inner(false);
    }

    // ── content ──

    fn build_content(
        self: &Rc<Self>,
        session: &Rc<Session>,
        renderer: &Rc<NativeRenderer>,
        config: &Value,
        section: Option<String>,
    ) {
        let Some(d) = self.d.upgrade() else { return };
        let cx = session.cx.clone();
        let registry = d
            .registry
            .borrow()
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(ChartTypeRegistry::builtin())));
        let hidden: HashSet<String> = renderer.hidden_settings().into_iter().collect();
        let mark_groups = renderer.mark_groups();
        *self.mark_choices.borrow_mut() = at(config, &["marks", "groups"])
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
                    .collect()
            })
            .unwrap_or_default();
        let facts = d.facts_source.borrow().as_ref().and_then(|f| f());
        let shown_style = renderer
            .read_feature("shownPriceStyle")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| js(config, &["series", "style"]));
        let unavailable: HashSet<String> = price_style_ids(&registry.borrow())
            .into_iter()
            .filter(|id| !price_style_applies(&registry.borrow(), id, facts.as_ref()))
            .collect();
        // `dark` and `light`, then every registered theme (read at build time, as the
        // original does), unless the host chose the list.
        let theme_choices: Vec<(String, String)> = match d.theme_choices.borrow().clone() {
            Some(choices) => choices,
            None => {
                let tr = d.tr();
                let mut all = vec![
                    ("dark".to_string(), tr.t("chart.settings.themeDark", "Dark")),
                    (
                        "light".to_string(),
                        tr.t("chart.settings.themeLight", "Light"),
                    ),
                ];
                all.extend(
                    crate::themes::registered()
                        .into_iter()
                        .map(|t| (t.id, t.label)),
                );
                all
            }
        };
        let chrome_bg = d.chrome_background();
        let host_sections = renderer.settings_sections();

        let shell_title = session.frame.title.clone();
        let (rail, scrim) = (
            cx.el("div", "vela-sd-rail"),
            cx.el("div", "vela-sd-railscrim"),
        );
        attr(&rail, "role", "tablist");
        attr(&rail, "aria-orientation", "vertical");
        let pane_host = cx.el("div", "vela-sd-pane");
        let shell = Rc::new(Shell {
            tabs: RefCell::new(Vec::new()),
            title: shell_title,
            default_title: cx.tr.t("chart.settings.chartSettings", "Chart settings"),
            rail: rail.clone(),
            scrim: scrim.clone(),
            mobile: session.mobile,
            dlg: Rc::downgrade(self),
        });
        {
            let sh = shell.clone();
            cx.on(&scrim, "click", move |_| sh.toggle_rail(Some(false)));
        }
        let mut b = Build {
            dlg: self.clone(),
            cx: cx.clone(),
            em: Rc::new(Emit {
                dlg: Rc::downgrade(self),
                renderer: renderer.clone(),
            }),
            config: config.clone(),
            hidden,
            registry: registry.clone(),
            panes: Vec::new(),
            skip: false,
            hint_info: |cx: &Cx, text: &str| controls::hint(cx, text, None, text, false),
        };
        let tr = cx.tr.clone();
        let t = |id: &'static str, en: &'static str| tr.t(id, en);

        // ══ SYMBOL ══
        b.tab(
            Some("symbol"),
            &t("chart.settings.symbol", "Symbol"),
            None,
            false,
        );
        b.title("symbol.type", &t("chart.settings.chartType", "Chart type"));
        let styles = price_style_ids(&registry.borrow());
        let groups: Rc<RefCell<HashMap<String, Element>>> = Rc::new(RefCell::new(HashMap::new()));
        let show_active: Rc<dyn Fn(&str)> = {
            let groups = groups.clone();
            Rc::new(move |style: &str| {
                let active = shared_group(style).to_string();
                for (k, el) in groups.borrow().iter() {
                    toggle_class(el, "vela-sd-hide", *k != active);
                }
            })
        };
        {
            let opts: Vec<SelOpt> = styles
                .iter()
                .map(|id| {
                    let mut o = SelOpt::new(
                        id.clone(),
                        cx.tr.m(&price_style_message(&registry.borrow(), id)),
                    );
                    if unavailable.contains(id) {
                        o.disabled = true;
                        o.title = Some(cx.tr.m(&price_style_unavailable()));
                    }
                    o
                })
                .collect();
            let (em, sh, sa) = (b.em.clone(), shell.clone(), show_active.clone());
            let row = select_row(
                &cx,
                &t("chart.settings.type", "Type"),
                &shown_style,
                &opts,
                Rc::new(move |v| {
                    em.path(&["series", "style"], json!(v));
                    sa(&v);
                    sh.sync_type_tabs(&v);
                }),
            );
            b.add("symbol.type", row);
        }
        let c = config.clone();
        let em = b.em.clone();
        let tog = |b: &mut Build,
                   parent: &Element,
                   id: &str,
                   label: &str,
                   tkey: [&'static str; 2],
                   keys: [[&'static str; 2]; 2],
                   labels: [String; 2]| {
            let _ = labels;
            let em = em.clone();
            let c = c.clone();
            let sw: Vec<Element> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let (k, em2) = (*k, em.clone());
                    let aria = format!("{label}: {}", labels[i]);
                    swatch(
                        &cx,
                        &aria,
                        &js(&c, &k),
                        Rc::new(move |v| em2.path(&k, json!(v))),
                    )
                    .el
                })
                .collect();
            let (get, set) = held(jb(&c, &tkey));
            let em3 = em.clone();
            let (row, _) = toggle_row(
                &cx,
                label,
                get,
                Rc::new(move |v| {
                    set(v);
                    em3.path(&tkey, json!(v))
                }),
                sw,
                None,
            );
            b.add_in(parent, id, row);
        };
        let up = t("chart.settings.colorUp", "Up color");
        let down = t("chart.settings.colorDown", "Down color");
        let ud = [up.clone(), down.clone()];

        // candles
        let candles = b.group("symbol.style.candles");
        groups
            .borrow_mut()
            .insert("candles".into(), candles.clone());
        b.add_in(
            &candles,
            "symbol.style.candles",
            title_el(&cx, &t("chart.settings.candles", "Candles")),
        );
        tog(
            &mut b,
            &candles,
            "symbol.style.candles.body",
            &t("chart.settings.body", "Body"),
            ["candles", "bodyVisible"],
            [["candles", "upColor"], ["candles", "downColor"]],
            ud.clone(),
        );
        tog(
            &mut b,
            &candles,
            "symbol.style.candles.borders",
            &t("chart.settings.borders", "Borders"),
            ["candles", "borderVisible"],
            [["candles", "borderUpColor"], ["candles", "borderDownColor"]],
            ud.clone(),
        );
        tog(
            &mut b,
            &candles,
            "symbol.style.candles.wick",
            &t("chart.settings.wick", "Wick"),
            ["candles", "wickVisible"],
            [["candles", "wickUpColor"], ["candles", "wickDownColor"]],
            ud.clone(),
        );
        let spacing = |b: &mut Build, parent: &Element, id: &str| {
            let em = b.em.clone();
            let row = number_row(
                &cx,
                &t("chart.settings.spacing", "Spacing"),
                jn(config, &["series", "spacing"]),
                0.1,
                10.0,
                0.1,
                Rc::new(move |v| em.path(&["series", "spacing"], json!(v))),
                None,
            );
            b.add_in(parent, id, row);
        };
        spacing(&mut b, &candles, "symbol.style.candles.spacing");

        // bars
        let bars = b.group("symbol.style.bars");
        groups.borrow_mut().insert("bars".into(), bars.clone());
        b.add_in(
            &bars,
            "symbol.style.bars",
            title_el(&cx, &t("chart.settings.bars", "Bars")),
        );
        let color =
            |b: &mut Build, parent: &Element, id: &str, label: &str, path: [&'static str; 2]| {
                let em = b.em.clone();
                let row = color_row(
                    &cx,
                    label,
                    &js(config, &path),
                    Rc::new(move |v| em.path(&path, json!(v))),
                );
                b.add_in(parent, id, row);
            };
        color(
            &mut b,
            &bars,
            "symbol.style.bars.up-color",
            &up,
            ["bars", "upColor"],
        );
        color(
            &mut b,
            &bars,
            "symbol.style.bars.down-color",
            &down,
            ["bars", "downColor"],
        );
        spacing(&mut b, &bars, "symbol.style.bars.spacing");
        let num = |b: &mut Build,
                   parent: &Element,
                   id: &str,
                   label: &str,
                   path: [&'static str; 2],
                   min: f64,
                   max: f64,
                   step: f64| {
            let em = b.em.clone();
            let row = number_row(
                &cx,
                label,
                jn(config, &path),
                min,
                max,
                step,
                Rc::new(move |v| em.path(&path, json!(v))),
                None,
            );
            b.add_in(parent, id, row);
        };
        let width = t("chart.settings.width", "Width");

        // line
        let line = b.group("symbol.style.line");
        groups.borrow_mut().insert("line".into(), line.clone());
        b.add_in(
            &line,
            "symbol.style.line",
            title_el(&cx, &t("chart.settings.line", "Line")),
        );
        color(
            &mut b,
            &line,
            "symbol.style.line.color",
            &t("chart.settings.color", "Color"),
            ["line", "color"],
        );
        num(
            &mut b,
            &line,
            "symbol.style.line.width",
            &width,
            ["line", "width"],
            1.0,
            10.0,
            1.0,
        );

        // area
        let area = b.group("symbol.style.area");
        groups.borrow_mut().insert("area".into(), area.clone());
        b.add_in(
            &area,
            "symbol.style.area",
            title_el(&cx, &t("chart.settings.area", "Area")),
        );
        color(
            &mut b,
            &area,
            "symbol.style.area.line-color",
            &t("chart.settings.lineColor", "Line color"),
            ["area", "lineColor"],
        );
        num(
            &mut b,
            &area,
            "symbol.style.area.width",
            &width,
            ["area", "width"],
            1.0,
            10.0,
            1.0,
        );
        color(
            &mut b,
            &area,
            "symbol.style.area.top-fill",
            &t("chart.settings.topFill", "Top fill"),
            ["area", "topColor"],
        );
        color(
            &mut b,
            &area,
            "symbol.style.area.bottom-fill",
            &t("chart.settings.bottomFill", "Bottom fill"),
            ["area", "bottomColor"],
        );

        // baseline
        let base = b.group("symbol.style.baseline");
        groups.borrow_mut().insert("baseline".into(), base.clone());
        b.add_in(
            &base,
            "symbol.style.baseline",
            title_el(&cx, &t("chart.settings.baseline", "Baseline")),
        );
        let swatch_row = |b: &mut Build,
                          parent: &Element,
                          id: &str,
                          label: String,
                          paths: Vec<[&'static str; 2]>| {
            let sw: Vec<Element> = paths
                .iter()
                .map(|p| {
                    let (p, em) = (*p, b.em.clone());
                    swatch(
                        &cx,
                        &label,
                        &js(config, &p),
                        Rc::new(move |v| em.path(&p, json!(v))),
                    )
                    .el
                })
                .collect();
            let row = row_with(&cx, &label, None, sw);
            b.add_in(parent, id, row);
        };
        swatch_row(
            &mut b,
            &base,
            "symbol.style.baseline.top-line",
            t("chart.settings.topLine", "Top line"),
            vec![["baseline", "topLineColor"]],
        );
        swatch_row(
            &mut b,
            &base,
            "symbol.style.baseline.bottom-line",
            t("chart.settings.bottomLine", "Bottom line"),
            vec![["baseline", "bottomLineColor"]],
        );
        swatch_row(
            &mut b,
            &base,
            "symbol.style.baseline.fill-top",
            t("chart.settings.fillTopArea", "Fill top area"),
            vec![["baseline", "topFillColor"], ["baseline", "topFillColor2"]],
        );
        swatch_row(
            &mut b,
            &base,
            "symbol.style.baseline.fill-bottom",
            t("chart.settings.fillBottomArea", "Fill bottom area"),
            vec![
                ["baseline", "bottomFillColor2"],
                ["baseline", "bottomFillColor"],
            ],
        );
        num(
            &mut b,
            &base,
            "symbol.style.baseline.base-level",
            &t("chart.settings.baseLevelPct", "Base level %"),
            ["baseline", "baselineLevel"],
            0.0,
            100.0,
            1.0,
        );
        num(
            &mut b,
            &base,
            "symbol.style.baseline.width",
            &width,
            ["baseline", "width"],
            1.0,
            10.0,
            1.0,
        );

        // columns
        let cols = b.group("symbol.style.columns");
        groups.borrow_mut().insert("columns".into(), cols.clone());
        b.add_in(
            &cols,
            "symbol.style.columns",
            title_el(&cx, &t("chart.settings.columns", "Columns")),
        );
        color(
            &mut b,
            &cols,
            "symbol.style.columns.up-color",
            &up,
            ["columns", "upColor"],
        );
        color(
            &mut b,
            &cols,
            "symbol.style.columns.down-color",
            &down,
            ["columns", "downColor"],
        );
        spacing(&mut b, &cols, "symbol.style.columns.spacing");

        // candle-based plugin styles keep candles in their own bag
        let own: Vec<String> = registry
            .borrow()
            .all()
            .iter()
            .filter(|def| has_own_candle_paint(&def.id, &registry.borrow()))
            .map(|d| d.id.clone())
            .collect();
        for id in own {
            let bag = at(config, &["chartTypes", &id]).clone();
            let color_of = |key: &str, shared: String| {
                bag.get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map_or(shared, str::to_string)
            };
            let bool_of =
                |key: &str, shared: bool| bag.get(key).and_then(Value::as_bool).unwrap_or(shared);
            let g = b.group(&format!("symbol.style.{id}"));
            groups.borrow_mut().insert(id.clone(), g.clone());
            b.add_in(
                &g,
                &format!("symbol.style.{id}"),
                title_el(&cx, &t("chart.settings.candles", "Candles")),
            );
            for (suffix, label, tkey, ukey, dkey, tshared, ushared, dshared) in [
                (
                    "body",
                    t("chart.settings.body", "Body"),
                    "candleBodyVisible",
                    "candleUpColor",
                    "candleDownColor",
                    jb(config, &["candles", "bodyVisible"]),
                    js(config, &["candles", "upColor"]),
                    js(config, &["candles", "downColor"]),
                ),
                (
                    "borders",
                    t("chart.settings.borders", "Borders"),
                    "candleBorderVisible",
                    "candleBorderUpColor",
                    "candleBorderDownColor",
                    jb(config, &["candles", "borderVisible"]),
                    js(config, &["candles", "borderUpColor"]),
                    js(config, &["candles", "borderDownColor"]),
                ),
                (
                    "wick",
                    t("chart.settings.wick", "Wick"),
                    "candleWickVisible",
                    "candleWickUpColor",
                    "candleWickDownColor",
                    jb(config, &["candles", "wickVisible"]),
                    js(config, &["candles", "wickUpColor"]),
                    js(config, &["candles", "wickDownColor"]),
                ),
            ] {
                let mk = |key: &'static str, val: String, aria: String| {
                    let (em, tid) = (b.em.clone(), id.clone());
                    swatch(
                        &cx,
                        &aria,
                        &val,
                        Rc::new(move |v| em.typed(&tid, key, json!(v))),
                    )
                    .el
                };
                let sw = vec![
                    mk(ukey, color_of(ukey, ushared), format!("{label}: {up}")),
                    mk(dkey, color_of(dkey, dshared), format!("{label}: {down}")),
                ];
                let (get, set) = held(bool_of(tkey, tshared));
                let (em, tid) = (b.em.clone(), id.clone());
                let (row, _) = toggle_row(
                    &cx,
                    &label,
                    get,
                    Rc::new(move |v| {
                        set(v);
                        em.typed(&tid, tkey, json!(v))
                    }),
                    sw,
                    None,
                );
                b.add_in(&g, &format!("symbol.style.{id}.{suffix}"), row);
            }
            spacing(&mut b, &g, &format!("symbol.style.{id}.spacing"));
        }
        show_active(&shown_style);

        // animation
        b.title(
            "symbol.animation",
            &t("chart.settings.animation", "Animation"),
        );
        let anim =
            |b: &mut Build, id: &str, label: String, path: [&'static str; 2], hint: String| {
                let em = b.em.clone();
                let info = (b.hint_info)(&cx, &hint);
                let (get, set) = held(jb(config, &path));
                let (row, _) = toggle_row(
                    &cx,
                    &label,
                    get,
                    Rc::new(move |v| {
                        set(v);
                        em.path(&path, json!(v))
                    }),
                    vec![],
                    Some(info),
                );
                b.add(id, row);
            };
        anim(
            &mut b,
            "symbol.animation.zoom",
            t("chart.settings.animateZoom", "Animate zoom"),
            ["animations", "zoom"],
            t(
                "chart.settings.animateZoomHint",
                "Glide the chart to each zoom step instead of jumping.",
            ),
        );
        anim(
            &mut b,
            "symbol.animation.pan",
            t("chart.settings.panMomentum", "Pan momentum"),
            ["animations", "pan"],
            t(
                "chart.settings.panMomentumHint",
                "Keep gliding briefly after a drag release, and ease scroll-to-latest and keyboard pans.",
            ),
        );
        anim(
            &mut b,
            "symbol.animation.autoscale",
            t("chart.settings.animatePriceScale", "Animate price scale"),
            ["animations", "autoscale"],
            t(
                "chart.settings.animatePriceScaleHint",
                "Glide the price scale to its new range while zooming or panning.",
            ),
        );
        anim(
            &mut b,
            "symbol.animation.price-changes",
            t(
                "chart.settings.animatePriceChanges",
                "Animate price changes",
            ),
            ["priceScale", "animateLastPrice"],
            t(
                "chart.settings.animatePriceChangesHint",
                "Glide the live bar to each new price instead of snapping.",
            ),
        );
        anim(
            &mut b,
            "symbol.animation.intro",
            t("chart.settings.revealOnLoad", "Reveal on load"),
            ["animations", "intro"],
            t(
                "chart.settings.revealOnLoadHint",
                "Draw the candles in when a chart first loads. Takes effect on the next load.",
            ),
        );

        // time zone
        b.title(
            "symbol.timezone",
            &t("chart.settings.timeZone", "Time zone"),
        );
        {
            let current = normalize_timezone(&js(config, &["timeScale", "timezone"])).to_string();
            let dynr = cx.tr.dyn_ref();
            let now = js_sys::Date::now();
            let mut opts: Vec<SelOpt> = timezones()
                .iter()
                .map(|z| {
                    let loc = cx.tr.m(&MessageLike::Message(z.name.clone()));
                    SelOpt::new(z.value, tz_menu_label(z.value, &loc, dynr, now))
                })
                .collect();
            if !opts.iter().any(|o| o.value == current) {
                opts.insert(
                    0,
                    SelOpt::new(
                        current.clone(),
                        tz_menu_label(&current, &current, dynr, now),
                    ),
                );
            }
            let em = b.em.clone();
            let row = select_row(
                &cx,
                &t("chart.settings.timeZone", "Time zone"),
                &current,
                &opts,
                Rc::new(move |v| em.path(&["timeScale", "timezone"], json!(v))),
            );
            b.add("symbol.timezone", row);
        }

        // ══ host sections + chart-type sections, in the original's order ══
        b.host_sections(&host_sections, "symbol");
        b.chart_type_sections("after-symbol");
        b.host_sections(&host_sections, "after-symbol");

        // ══ SCALES AND LINES ══
        b.tab(
            Some("scales"),
            &t("chart.settings.scalesAndLines", "Scales and lines"),
            None,
            false,
        );
        b.title(
            "scales.price-scale",
            &t("chart.settings.priceScale", "Price scale"),
        );
        {
            let mode = if jb(config, &["priceScale", "log"]) {
                "log".to_string()
            } else {
                js(config, &["priceScale", "mode"])
            };
            let opts = vec![
                SelOpt::new("price", t("chart.settings.regular", "Regular")),
                SelOpt::new("percent", t("chart.settings.percent", "Percent")),
                SelOpt::new(
                    "indexed",
                    t("chart.settings.indexedTo100", "Indexed to 100"),
                ),
                SelOpt::new("log", t("chart.settings.logarithmic", "Logarithmic")),
            ];
            let em = b.em.clone();
            let row = select_row(
                &cx,
                &t("chart.settings.mode", "Mode"),
                &mode,
                &opts,
                Rc::new(move |v| {
                    em.send(json!({ "priceScale": if v == "log" { json!({"mode": "price", "log": true}) } else { json!({"mode": v, "log": false}) } }))
                }),
            );
            b.add("scales.price-scale.mode", row);
        }
        let boolrow = |b: &mut Build, id: &str, label: String, path: [&'static str; 2]| {
            let em = b.em.clone();
            let (get, set) = held(jb(config, &path));
            let (row, _) = toggle_row(
                &cx,
                &label,
                get,
                Rc::new(move |v| {
                    set(v);
                    em.path(&path, json!(v))
                }),
                vec![],
                None,
            );
            b.add(id, row);
        };
        boolrow(
            &mut b,
            "scales.price-scale.invert",
            t("chart.settings.invertScale", "Invert scale"),
            ["priceScale", "invert"],
        );
        boolrow(
            &mut b,
            "scales.price-scale.price-only",
            t("chart.settings.scalePriceOnly", "Scale price chart only"),
            ["priceScale", "priceOnly"],
        );
        boolrow(
            &mut b,
            "scales.price-scale.free-pan",
            t("chart.settings.freePan", "Free pan"),
            ["priceScale", "freePan"],
        );
        b.add("scales.price-scale", cx.el("div", "vela-sd-sep"));
        boolrow(
            &mut b,
            "scales.price-scale.last-price-line",
            t("chart.settings.lastPriceLine", "Last price line"),
            ["priceScale", "currentPriceLine"],
        );
        boolrow(
            &mut b,
            "scales.price-scale.last-price-label",
            t("chart.settings.lastPriceLabel", "Last price label"),
            ["priceScale", "priceLabel"],
        );
        boolrow(
            &mut b,
            "scales.price-scale.countdown",
            t(
                "chart.settings.countdownToBarClose",
                "Countdown to bar close",
            ),
            ["priceScale", "countdown"],
        );
        boolrow(
            &mut b,
            "scales.price-scale.axis-labels",
            t("chart.settings.axisLabels", "Axis labels"),
            ["priceScale", "labelsVisible"],
        );
        {
            let em = b.em.clone();
            let row = color_row(
                &cx,
                &t("chart.settings.scaleBorderColor", "Scale border color"),
                &js(config, &["priceScale", "borderColor"]),
                Rc::new(move |v| em.path(&["priceScale", "borderColor"], json!(v))),
            );
            b.add("scales.price-scale.border-color", row);
        }
        b.title(
            "scales.crosshair",
            &t("chart.settings.crosshair", "Crosshair"),
        );
        {
            let em = b.em.clone();
            let row = color_row(
                &cx,
                &t("chart.settings.color", "Color"),
                &js(config, &["crosshair", "color"]),
                Rc::new(move |v| em.path(&["crosshair", "color"], json!(v))),
            );
            b.add("scales.crosshair.color", row);
            let em = b.em.clone();
            let row = number_row(
                &cx,
                &width,
                jn(config, &["crosshair", "width"]),
                0.5,
                8.0,
                0.5,
                Rc::new(move |v| em.path(&["crosshair", "width"], json!(v))),
                None,
            );
            b.add("scales.crosshair.width", row);
            let opts = vec![
                SelOpt::new("solid", t("chart.settings.solid", "Solid")),
                SelOpt::new("dashed", t("chart.settings.dashed", "Dashed")),
                SelOpt::new("dotted", t("chart.settings.dotted", "Dotted")),
            ];
            let em = b.em.clone();
            let row = select_row(
                &cx,
                &t("chart.settings.style", "Style"),
                &js(config, &["crosshair", "style"]),
                &opts,
                Rc::new(move |v| em.path(&["crosshair", "style"], json!(v))),
            );
            b.add("scales.crosshair.style", row);
        }

        // ══ CANVAS ══
        b.tab(
            Some("canvas"),
            &t("chart.settings.canvas", "Canvas"),
            None,
            false,
        );
        b.title(
            "canvas.background",
            &t("chart.settings.backgroundAndText", "Background & text"),
        );
        for (id, label, path) in [
            (
                "canvas.background.color",
                t("chart.settings.background", "Background"),
                ["layout", "background"],
            ),
            (
                "canvas.background.text-color",
                t("chart.settings.textColor", "Text color"),
                ["layout", "textColor"],
            ),
        ] {
            let em = b.em.clone();
            let row = color_row(
                &cx,
                &label,
                &js(config, &path),
                Rc::new(move |v| em.path(&path, json!(v))),
            );
            b.add(id, row);
        }
        {
            let em = b.em.clone();
            let row = number_row(
                &cx,
                &t("chart.settings.textSize", "Text size"),
                jn(config, &["layout", "fontSize"]),
                6.0,
                32.0,
                1.0,
                Rc::new(move |v| em.path(&["layout", "fontSize"], json!(v))),
                None,
            );
            b.add("canvas.background.text-size", row);
            let em = b.em.clone();
            let row = color_row(
                &cx,
                &t("chart.settings.paneSeparatorColor", "Panel separator color"),
                &js(config, &["panes", "separatorColor"]),
                Rc::new(move |v| em.path(&["panes", "separatorColor"], json!(v))),
            );
            b.add("canvas.background.pane-separator", row);
        }
        b.title("canvas.grid", &t("chart.settings.grid", "Grid"));
        for (id, label, vis, col) in [
            (
                "canvas.grid.vertical",
                t("chart.settings.vertical", "Vertical"),
                ["grid", "vertLines", "visible"],
                ["grid", "vertLines", "color"],
            ),
            (
                "canvas.grid.horizontal",
                t("chart.settings.horizontal", "Horizontal"),
                ["grid", "horzLines", "visible"],
                ["grid", "horzLines", "color"],
            ),
        ] {
            let (em, em2) = (b.em.clone(), b.em.clone());
            let sw = swatch(
                &cx,
                &label,
                &js(config, &col),
                Rc::new(move |v| em2.path(&col, json!(v))),
            )
            .el;
            let (get, set) = held(jb(config, &vis));
            let (row, _) = toggle_row(
                &cx,
                &label,
                get,
                Rc::new(move |v| {
                    set(v);
                    em.path(&vis, json!(v))
                }),
                vec![sw],
                None,
            );
            b.add(id, row);
        }
        b.title("canvas.margins", &t("chart.settings.margins", "Margins"));
        for (id, label, key, max, unit) in [
            (
                "canvas.margins.top",
                t("chart.settings.top", "Top"),
                "top",
                40.0,
                None,
            ),
            (
                "canvas.margins.bottom",
                t("chart.settings.bottom", "Bottom"),
                "bottom",
                40.0,
                None,
            ),
            (
                "canvas.margins.right",
                t("chart.settings.right", "Right"),
                "right",
                200.0,
                Some(msg(
                    "chart.settings.unitBars",
                    "{count, plural, one {bar} other {bars}}",
                )),
            ),
        ] {
            let em = b.em.clone();
            let unit = unit.or_else(|| (key != "right").then(|| MessageLike::from("%")));
            let row = number_row(
                &cx,
                &label,
                jn(config, &["margins", key]),
                0.0,
                max,
                1.0,
                Rc::new(move |v| em.path(&["margins", key], json!(v))),
                unit,
            );
            b.add(id, row);
        }
        if !theme_choices.is_empty() {
            b.title("canvas.theme", &t("chart.settings.theme", "Theme"));
            // The selection is the app theme in force (a registered theme's id when the app
            // theme is one), else the stable surface's darkness; never the plot background.
            let current = d
                .theme_current
                .borrow()
                .clone()
                .and_then(|f| f())
                .or_else(|| {
                    crate::themes::shared()
                        .borrow()
                        .registered_theme_id_of(&renderer.app_theme())
                })
                .unwrap_or_else(|| {
                    if vela_dark(&chrome_bg) {
                        "dark"
                    } else {
                        "light"
                    }
                    .to_string()
                });
            let opts: Vec<SelOpt> = theme_choices
                .iter()
                .map(|(id, l)| SelOpt::new(id.clone(), l.clone()))
                .collect();
            let (r, dd) = (renderer.clone(), d.me.clone());
            let row = select_row(
                &cx,
                &t("chart.settings.colorTheme", "Color theme"),
                &current,
                &opts,
                Rc::new(move |v| {
                    r.notify_theme_select(&v);
                    if let Some(d) = dd.upgrade() {
                        d.refresh_open();
                    }
                }),
            );
            b.add("canvas.theme", row);
        }

        // ══ EVENTS ══
        if !mark_groups.is_empty() {
            b.tab(
                Some(MARKS_SETTINGS_ID),
                &t("chart.settings.events", "Events"),
                None,
                false,
            );
            b.title(
                MARKS_GROUPS_SETTINGS_ID,
                &t("chart.settings.visibleEvents", "Visible events"),
            );
            b.events_rows(&mark_groups);
        }
        b.chart_type_sections("end");
        b.host_sections(&host_sections, "end");

        // ── split into rail + panes ──
        let built = b.finish();
        let mut tabs = built;
        tabs.retain(|t| t.pane.first_child().is_some());
        let sh = shell.clone();
        for (i, t) in tabs.iter().enumerate() {
            let sh2 = sh.clone();
            cx.on(&t.button, "click", move |_| sh2.activate(i));
            let sh3 = sh.clone();
            cx.on(&t.button, "keydown", move |ev| {
                let key = dlg_key(&ev);
                let n = sh3.tabs.borrow().len();
                let visible: Vec<usize> = (0..n)
                    .filter(|i| {
                        sh3.tabs.borrow()[*i]
                            .button
                            .unchecked_ref::<web_sys::HtmlElement>()
                            .style()
                            .get_property_value("display")
                            .unwrap_or_default()
                            != "none"
                    })
                    .collect();
                let Some(pos) = sh3
                    .shown_index()
                    .and_then(|s| visible.iter().position(|v| *v == s))
                else {
                    return;
                };
                let next = match key.as_str() {
                    "ArrowDown" | "ArrowRight" => Some(visible[(pos + 1) % visible.len()]),
                    "ArrowUp" | "ArrowLeft" => {
                        Some(visible[(pos + visible.len() - 1) % visible.len()])
                    }
                    "Home" => visible.first().copied(),
                    "End" => visible.last().copied(),
                    _ => None,
                };
                if let Some(next) = next {
                    crate::dom::prevent_default(&ev);
                    sh3.activate(next);
                    let btn = sh3.tabs.borrow()[next].button.clone();
                    crate::dom::vela_focus(&btn);
                }
            });
            let _ = rail.append_child(&t.button);
            let _ = pane_host.append_child(&t.pane);
        }
        *shell.tabs.borrow_mut() = tabs;
        // Which tab: the one asked for, else the active style's own tab, else Symbol.
        let wanted = match &section {
            Some(s) => shell
                .tabs
                .borrow()
                .iter()
                .position(|t| t.title.eq_ignore_ascii_case(s)),
            None => shell
                .tabs
                .borrow()
                .iter()
                .position(|t| t.style.as_deref() == Some(shown_style.as_str()) && !t.sub),
        };
        shell.activate(wanted.unwrap_or(0));
        shell.sync_type_tabs(&shown_style);
        let shell_el = cx.el("div", "vela-sd-shell");
        let _ = shell_el.append_child(&rail);
        let _ = shell_el.append_child(&pane_host);
        let _ = shell_el.append_child(&scrim);
        let _ = session.frame.body.append_child(&shell_el);
        *session.shell.borrow_mut() = Some(shell);
    }
}

fn vela_dark(bg: &str) -> bool {
    super::color::is_dark(bg)
}

/// Wire the mobile burger of a session's header.
fn cx_burger(session: &Rc<Session>, f: impl Fn() + 'static) {
    session.cx.on(&session.frame.burger, "click", move |_| f());
    attr(
        &session.frame.burger,
        "aria-label",
        &session.cx.tr.t("chart.settings.sections", "Sections"),
    );
    attr(
        &session.frame.burger,
        "title",
        &session.cx.tr.t("chart.settings.sections", "Sections"),
    );
}

// ── the build state ──

struct Build {
    dlg: Rc<SettingsDialog>,
    cx: Cx,
    em: Rc<Emit>,
    config: Value,
    hidden: HashSet<String>,
    registry: Rc<RefCell<ChartTypeRegistry>>,
    panes: Vec<(TabH, Option<Element>)>,
    skip: bool,
    hint_info: fn(&Cx, &str) -> Element,
}

impl Build {
    fn hid(&self, id: &str) -> bool {
        settings_id_hidden(id, &self.hidden)
    }

    /// Start a tab. A tab whose id the policy hides drops every row until the next tab.
    fn tab(&mut self, id: Option<&str>, label: &str, style: Option<(&str, bool)>, sub: bool) {
        self.skip = id.is_some_and(|i| self.hid(i));
        let pane = self.cx.el("div", "vela-sd-panel");
        attr(&pane, "role", "tabpanel");
        set_style(&pane, "display", "none");
        let button = self.cx.el(
            "button",
            if sub {
                "vela-sd-tab vela-sd-tab-sub"
            } else {
                "vela-sd-tab"
            },
        );
        attr(&button, "type", "button");
        attr(&button, "role", "tab");
        let bid = self.cx.uid("vela-sd-tab");
        button.set_id(&bid);
        attr(&pane, "aria-labelledby", &bid);
        button.set_text_content(Some(label));
        let pid = self.cx.uid("vela-sd-panel");
        pane.set_id(&pid);
        attr(&button, "aria-controls", &pid);
        self.panes.push((
            TabH {
                title: match id {
                    Some("symbol") => "Symbol".to_string(),
                    Some("scales") => "Scales and lines".to_string(),
                    Some("canvas") => "Canvas".to_string(),
                    Some(MARKS_SETTINGS_ID) => "Events".to_string(),
                    _ => label_identity(label),
                },
                label: label.to_string(),
                pane,
                button,
                style: style.map(|s| s.0.to_string()),
                always: style.is_some_and(|s| s.1),
                sub,
            },
            None,
        ));
    }

    /// The tab's English identity (what a `section` argument names) differs from its label
    /// when translated: record it.
    fn set_identity(&mut self, identity: &str) {
        if let Some((t, _)) = self.panes.last_mut() {
            t.title = identity.to_string();
        }
    }

    fn grid(&mut self) -> Element {
        let cx = self.cx.clone();
        let (t, g) = self.panes.last_mut().expect("a tab is open");
        g.get_or_insert_with(|| {
            let grid = cx.el("div", "vela-sd-grid");
            let _ = t.pane.append_child(&grid);
            grid
        })
        .clone()
    }

    fn add(&mut self, id: &str, el: Element) {
        if self.skip || self.hid(id) {
            return;
        }
        attr(&el, "data-sd-id", id);
        let g = self.grid();
        let _ = g.append_child(&el);
    }

    fn add_in(&mut self, parent: &Element, id: &str, el: Element) {
        if self.skip || self.hid(id) {
            return;
        }
        attr(&el, "data-sd-id", id);
        let _ = parent.append_child(&el);
    }

    fn title(&mut self, id: &str, text: &str) {
        let el = title_el(&self.cx, text);
        self.add(id, el);
    }

    /// A `display:contents` group stamped with `id`, appended to the grid unless hidden.
    fn group(&mut self, id: &str) -> Element {
        let g = group_el(&self.cx);
        self.add(id, g.clone());
        g
    }

    fn finish(self) -> Vec<TabH> {
        self.panes.into_iter().map(|(t, _)| t).collect()
    }

    fn events_rows(&mut self, groups: &[MarkGroup]) {
        let rows = mark_group_rows(groups);
        let by_id: Rc<RefCell<HashMap<String, Element>>> = Rc::new(RefCell::new(HashMap::new()));
        let all: Rc<Vec<MarkGroup>> = Rc::new(groups.to_vec());
        let dlg = self.dlg.clone();
        let refresh: Rc<dyn Fn()> = {
            let (by_id, all, dlg) = (by_id.clone(), all.clone(), dlg.clone());
            Rc::new(move || {
                let choices = dlg.mark_choices.borrow().clone();
                for g in all.iter() {
                    if let (Some(parent), Some(el)) = (&g.parent, by_id.borrow().get(&g.id)) {
                        let on = mark_group_visible(&choices, Some(parent), &all);
                        toggle_class(el, "vela-sd-soft", !on);
                    }
                }
            })
        };
        for row in rows {
            let g = row.group.clone();
            let choices = self.dlg.mark_choices.borrow().clone();
            let own = mark_group_own_visible(&choices, &g.id, groups);
            let (get, set) = held(own);
            let (em, dlg2, refresh2, gid) = (
                self.em.clone(),
                self.dlg.clone(),
                refresh.clone(),
                g.id.clone(),
            );
            let (el, _) = toggle_row(
                &self.cx,
                &g.label,
                get,
                Rc::new(move |v| {
                    set(v);
                    dlg2.mark_choices.borrow_mut().insert(gid.clone(), v);
                    em.send(json!({ "marks": { "groups": { gid.clone(): v } } }));
                    refresh2();
                }),
                vec![],
                None,
            );
            if row.depth > 0 {
                toggle_class(&el, "vela-sd-nested", true);
                set_style(&el, "--vela-sd-depth", &row.depth.to_string());
            }
            by_id.borrow_mut().insert(g.id.clone(), el.clone());
            self.add(&mark_group_settings_id(&g.id), el);
        }
        refresh();
    }

    // ── host sections ──

    fn host_sections(
        &mut self,
        sections: &[vela_core::ports::chart_renderer::SettingsSection],
        placement: &str,
    ) {
        for hs in sections {
            let p = hs.placement_of();
            if p != placement {
                continue;
            }
            let rows: Vec<HRow> = hs
                .rows
                .iter()
                .filter(|r| r.get("kind").and_then(Value::as_str) != Some("placement"))
                .map(|r| HRow {
                    src: HostRowIdSource {
                        kind: r
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        label: MessageLike::from(
                            r.get("label")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        id: r.get("id").and_then(Value::as_str).map(str::to_string),
                    },
                    json: r.clone(),
                })
                .collect();
            let scope = host_section_id(&HostSectionIdSource {
                title: hs.title.clone(),
                rows: rows.iter().map(|r| r.src.clone()).collect(),
                id: hs.id.clone(),
            });
            if settings_id_hidden(&scope, &self.hidden) {
                continue;
            }
            let rows = filter_hidden_host_rows(&rows, &scope, &self.hidden);
            if rows.is_empty() {
                continue;
            }
            let title = self.cx.tr.m(&hs.title);
            if placement == "symbol" {
                let el = title_el(&self.cx, &title);
                let g = self.grid_of_first();
                let _ = g.append_child(&el);
            } else {
                self.tab(None, &title, None, false);
                self.set_identity(&source_text(&hs.title));
            }
            for r in rows {
                let el = self.host_row(&scope, &r);
                let g = if placement == "symbol" {
                    self.grid_of_first()
                } else {
                    self.grid()
                };
                let _ = g.append_child(&el);
            }
        }
    }

    fn grid_of_first(&mut self) -> Element {
        let cx = self.cx.clone();
        let (t, g) = &mut self.panes[0];
        g.get_or_insert_with(|| {
            let grid = cx.el("div", "vela-sd-grid");
            let _ = t.pane.append_child(&grid);
            grid
        })
        .clone()
    }

    fn host_row(&self, scope: &str, r: &HRow) -> Element {
        let cx = &self.cx;
        let label = r
            .json
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let row_id = r
            .src
            .id
            .clone()
            .unwrap_or_else(|| settings_id_slug(&r.src.label));
        let wrapper = self.dlg.d.upgrade().map(|d| d.wrapper.clone());
        let announce = {
            let (scope, row_id, wrapper) = (scope.to_string(), row_id.clone(), wrapper);
            Rc::new(move |value: Value| {
                if let Some(w) = &wrapper {
                    dlg_dispatch(
                        w,
                        "vela-host-setting",
                        &json!({ "section": scope, "row": row_id, "value": value }).to_string(),
                    );
                }
            })
        };
        match r.src.kind.as_str() {
            "heading" => title_el(cx, &label),
            "toggle" => {
                let (get, set) = held(
                    r.json
                        .get("value")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
                toggle_row(
                    cx,
                    &label,
                    get,
                    Rc::new(move |v| {
                        set(v);
                        announce(json!(v))
                    }),
                    vec![],
                    None,
                )
                .0
            }
            "color" => {
                let value = r
                    .json
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                color_row(cx, &label, &value, Rc::new(move |v| announce(json!(v))))
            }
            _ => {
                let opts: Vec<SelOpt> = r
                    .json
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| match o {
                                Value::String(s) => Some(SelOpt::new(s.clone(), s.clone())),
                                Value::Array(p) => {
                                    Some(SelOpt::new(p.first()?.as_str()?, p.get(1)?.as_str()?))
                                }
                                Value::Object(m) => Some(SelOpt::new(
                                    m.get("value")?.as_str()?,
                                    m.get("label")?.as_str()?,
                                )),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let value = r
                    .json
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                select_row(
                    cx,
                    &label,
                    &value,
                    &opts,
                    Rc::new(move |v| announce(json!(v))),
                )
            }
        }
    }

    // ── chart-type sections ──

    fn chart_type_sections(&mut self, placement: &str) {
        let defs: Vec<(String, ChartTypeSettingsSection)> = self
            .registry
            .borrow()
            .all()
            .iter()
            .filter_map(|d| d.settings.clone().map(|s| (d.id.clone(), s)))
            .collect();
        for (id, section) in defs {
            let p = match section.placement {
                SettingsPlacement::AfterSymbol => "after-symbol",
                SettingsPlacement::End => "end",
            };
            if p != placement || self.hid(&format!("type:{id}")) {
                continue;
            }
            self.chart_type_section(&id, &section);
        }
    }

    fn chart_type_section(&mut self, type_id: &str, section: &ChartTypeSettingsSection) {
        let cx = self.cx.clone();
        let always = section.visibility == SettingsVisibility::Always;
        let label = cx.tr.m(&section.title);
        self.tab(None, &label, Some((type_id, always)), false);
        self.set_identity(&source_text(&section.title));

        let values: Map<String, Value> = at(&self.config, &["chartTypes", type_id])
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut bag: Map<String, Value> = Map::new();
        let seed = |bag: &mut Map<String, Value>, rows: &[SettingsRowDescriptor]| {
            for r in rows {
                for k in settings_row_value_keys(r) {
                    let stored = values.get(&k.key);
                    let ok = matches!(
                        (k.value_type, stored),
                        (SettingsValueType::Boolean, Some(Value::Bool(_)))
                            | (SettingsValueType::Number, Some(Value::Number(_)))
                            | (SettingsValueType::String, Some(Value::String(_)))
                    );
                    let v = if ok {
                        stored.cloned().unwrap_or(Value::Null)
                    } else {
                        match &k.defval {
                            SettingsValue::Bool(b) => json!(b),
                            SettingsValue::Text(s) => json!(s),
                            SettingsValue::Number(n) => json!(n),
                        }
                    };
                    bag.insert(k.key, v);
                }
            }
        };
        seed(&mut bag, &section.rows);
        for i in &section.instances {
            seed(&mut bag, &i.rows);
        }
        for s in &section.subsections {
            seed(&mut bag, &s.rows);
        }
        let mut enable_keys: Vec<&String> = section
            .instances
            .iter()
            .filter_map(|i| i.enable_key.as_ref())
            .collect();
        enable_keys.extend(
            section
                .subsections
                .iter()
                .filter_map(|s| s.enable_key.as_ref()),
        );
        for k in enable_keys {
            if !bag.contains_key(k) {
                bag.insert(k.clone(), json!(values.get(k) == Some(&Value::Bool(true))));
            }
        }
        let bag = Rc::new(RefCell::new(bag));
        let refreshers: Rc<RefCell<Vec<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(Vec::new()));
        let put: Rc<dyn Fn(&str, Value)> = {
            let (bag, em, tid, refreshers) = (
                bag.clone(),
                self.em.clone(),
                type_id.to_string(),
                refreshers.clone(),
            );
            Rc::new(move |key, v| {
                bag.borrow_mut().insert(key.to_string(), v.clone());
                em.typed(&tid, key, v);
                let rs: Vec<_> = refreshers.borrow().clone();
                for r in rs {
                    r();
                }
            })
        };
        let ctx = TypeCtx {
            cx: cx.clone(),
            bag: bag.clone(),
            put,
            refreshers: refreshers.clone(),
            dlg: self.dlg.clone(),
        };
        let scope = format!("type:{type_id}");
        if !section.instances.is_empty() {
            let instances: Vec<ChartTypeSettingsInstance> = section
                .instances
                .iter()
                .map(|i| ChartTypeSettingsInstance {
                    rows: filter_hidden_rows(&i.rows, &scope, &self.hidden),
                    ..i.clone()
                })
                .collect();
            let block = ctx.instances_block(type_id, &instances);
            self.append_pane(block);
        } else if !section.rows.is_empty() {
            let rows = filter_hidden_rows(&section.rows, &scope, &self.hidden);
            if section.layout == SettingsLayout::Grouped {
                let block = ctx.grouped_rows(&format!("{type_id}/rows"), &rows, None);
                self.append_pane(block);
            } else {
                let grid = self.grid();
                ctx.flat_rows(&rows, &grid);
            }
        }
        for sub in &section.subsections {
            let sid = format!("{scope}.{}", settings_id_slug(&sub.title));
            if settings_id_hidden(&sid, &self.hidden) {
                continue;
            }
            let rows = filter_hidden_rows(&sub.rows, &scope, &self.hidden);
            if rows.is_empty() {
                continue;
            }
            let label = cx.tr.m(&sub.title);
            self.tab(None, &label, Some((type_id, always)), true);
            self.set_identity(&source_text(&sub.title));
            let block = ctx.grouped_rows(
                &format!("{type_id}/{}", source_text(&sub.title)),
                &rows,
                sub.enable_key.clone(),
            );
            self.append_pane(block);
        }
        let rs: Vec<_> = refreshers.borrow().clone();
        for r in rs {
            r();
        }
    }

    fn append_pane(&mut self, el: Element) {
        let (t, _) = self.panes.last_mut().expect("a tab is open");
        let _ = t.pane.append_child(&el);
    }
}

fn label_identity(label: &str) -> String {
    label.to_string()
}

// ── chart-type rows ──

struct TypeCtx {
    cx: Cx,
    bag: Rc<RefCell<Map<String, Value>>>,
    put: Rc<dyn Fn(&str, Value)>,
    refreshers: Rc<RefCell<Vec<Rc<dyn Fn()>>>>,
    dlg: Rc<SettingsDialog>,
}

fn select_options(opts: &[SettingsSelectOption], tr: &Tr) -> Vec<SelOpt> {
    opts.iter()
        .map(|o| match o {
            SettingsSelectOption::Plain(s) => SelOpt::new(s.clone(), s.clone()),
            SettingsSelectOption::Pair { value, label } => SelOpt::new(value.clone(), tr.m(label)),
        })
        .collect()
}

impl TypeCtx {
    fn add_refresher(&self, f: Rc<dyn Fn()>) {
        self.refreshers.borrow_mut().push(f);
    }

    fn inline_control(
        &self,
        c: &SettingsInlineControl,
        compact: bool,
    ) -> (Element, Option<Rc<dyn Fn()>>) {
        let cx = &self.cx;
        match c {
            SettingsInlineControl::Hint { text } => {
                (cx.text("span", "vela-sd-hint", &cx.tr.m(text)), None)
            }
            SettingsInlineControl::Color {
                key, label, defval, ..
            } => {
                let (bag, put, k, d) = (
                    self.bag.clone(),
                    self.put.clone(),
                    key.clone(),
                    defval.clone(),
                );
                let k2 = k.clone();
                let ctl = controls::color_field(
                    cx,
                    &cx.uid("vela-sd"),
                    &cx.tr.m(label),
                    false,
                    Rc::new(move || {
                        bag.borrow()
                            .get(&k)
                            .and_then(Value::as_str)
                            .unwrap_or(&d)
                            .to_string()
                    }),
                    Rc::new(move |v| put(&k2, json!(v))),
                );
                let s = ctl.sync.clone();
                (ctl.el, Some(s))
            }
            SettingsInlineControl::Width {
                key, label, defval, ..
            } => {
                let (bag, put, k, d) = (self.bag.clone(), self.put.clone(), key.clone(), *defval);
                let k2 = k.clone();
                let current = bag.borrow().get(key).and_then(Value::as_f64).unwrap_or(d);
                let mut opts: Vec<SelOpt> = (1..=5)
                    .map(|n| SelOpt::new(n.to_string(), format!("{n} px")))
                    .collect();
                if !(1..=5).any(|n| f64::from(n) == current) {
                    opts.insert(0, SelOpt::new(current.to_string(), format!("{current} px")));
                }
                let ctl = controls::select(
                    cx,
                    &cx.uid("vela-sd"),
                    &cx.tr.m(label),
                    &opts,
                    Rc::new(move || {
                        let v = bag.borrow().get(&k).and_then(Value::as_f64).unwrap_or(d);
                        if v == v.trunc() {
                            format!("{}", v as i64)
                        } else {
                            v.to_string()
                        }
                    }),
                    Rc::new(move |v| put(&k2, json!(v.parse::<f64>().unwrap_or(d)))),
                );
                let s = ctl.sync.clone();
                (ctl.el, Some(s))
            }
            SettingsInlineControl::Select {
                key,
                label,
                options,
                defval,
                ..
            } => {
                let (bag, put, k, d) = (
                    self.bag.clone(),
                    self.put.clone(),
                    key.clone(),
                    defval.clone(),
                );
                let k2 = k.clone();
                let ctl = controls::select(
                    cx,
                    &cx.uid("vela-sd"),
                    &cx.tr.m(label),
                    &select_options(options, &cx.tr),
                    Rc::new(move || {
                        bag.borrow()
                            .get(&k)
                            .and_then(Value::as_str)
                            .unwrap_or(&d)
                            .to_string()
                    }),
                    Rc::new(move |v| put(&k2, json!(v))),
                );
                let s = ctl.sync.clone();
                (ctl.el, Some(s))
            }
            SettingsInlineControl::Number {
                key,
                label,
                defval,
                min,
                max,
                step,
                placeholder,
                ..
            } => {
                let (bag, put, k, d) = (self.bag.clone(), self.put.clone(), key.clone(), *defval);
                let k2 = k.clone();
                let ctl = controls::number(
                    cx,
                    &cx.uid("vela-sd"),
                    NumOpts {
                        min: *min,
                        max: *max,
                        step: Some(step.unwrap_or(1.0)),
                        live: true,
                        compact,
                        aria: cx.tr.m(label),
                        placeholder: placeholder.as_ref().map(|p| cx.tr.m(p)),
                        empty_value: placeholder.as_ref().map(|_| d),
                        clamp: placeholder.is_some(),
                        ..NumOpts::default()
                    },
                    Rc::new(move || bag.borrow().get(&k).and_then(Value::as_f64).unwrap_or(d)),
                    Rc::new(move |v| put(&k2, json!(v))),
                );
                let s = ctl.sync.clone();
                (ctl.el, Some(s))
            }
        }
    }

    fn type_row(&self, r: &SettingsRowDescriptor) -> Element {
        let cx = &self.cx;
        let Some(n) = normalize_settings_row(r) else {
            return cx.el("span", "");
        };
        let has_toggle = n.toggle.is_some();
        let mut els: Vec<Element> = Vec::new();
        for c in &n.controls {
            let (el, sync) = self.inline_control(c, has_toggle);
            if let Some(sync) = sync {
                self.add_refresher(sync);
            }
            let when = match c {
                SettingsInlineControl::Number { when, .. }
                | SettingsInlineControl::Color { when, .. }
                | SettingsInlineControl::Width { when, .. }
                | SettingsInlineControl::Select { when, .. } => when.clone(),
                SettingsInlineControl::Hint { .. } => None,
            };
            if when.is_some() {
                let (bag, el2) = (self.bag.clone(), el.clone());
                self.add_refresher(Rc::new(move || {
                    set_style(
                        &el2,
                        "display",
                        if when_visible(&when, &bag) {
                            ""
                        } else {
                            "none"
                        },
                    );
                }));
            }
            els.push(el);
        }
        let label = cx.tr.m(&n.label);
        if let Some(t) = &n.toggle {
            let (bag, put, k) = (self.bag.clone(), self.put.clone(), t.key.clone());
            let k2 = k.clone();
            let (row, sync) = toggle_row(
                cx,
                &label,
                Rc::new(move || bag.borrow().get(&k).and_then(Value::as_bool) == Some(true)),
                Rc::new(move |v| put(&k2, json!(v))),
                els,
                None,
            );
            self.add_refresher(sync);
            return row;
        }
        let for_id = els.first().map(|e| e.id()).filter(|s| !s.is_empty());
        row_with(cx, &label, for_id.as_deref(), els)
    }

    fn flat_rows(&self, rows: &[SettingsRowDescriptor], grid: &Element) {
        let mut entries: Vec<(Element, Option<SettingsRowWhen>)> = Vec::new();
        for r in rows {
            let (el, when) = match r {
                SettingsRowDescriptor::Heading { label, when }
                | SettingsRowDescriptor::Header { label, when } => {
                    (title_el(&self.cx, &self.cx.tr.m(label)), when.clone())
                }
                other => (self.type_row(other), row_when(other)),
            };
            let _ = grid.append_child(&el);
            entries.push((el, when));
        }
        let bag = self.bag.clone();
        self.add_refresher(Rc::new(move || {
            for (el, when) in &entries {
                toggle_class(el, "vela-sd-hide", !when_visible(when, &bag));
            }
        }));
    }

    fn instances_block(&self, type_id: &str, instances: &[ChartTypeSettingsInstance]) -> Element {
        let cx = self.cx.clone();
        let wrap = cx.el("div", "");
        let strip = cx.el("div", "vela-sd-itabs");
        attr(&strip, "role", "tablist");
        if instances.len() > 1 || instances.first().is_some_and(|i| i.enable_key.is_some()) {
            let _ = wrap.append_child(&strip);
        }
        let contents: Vec<Element> = instances
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let c = self.grouped_rows(&format!("{type_id}/#{i}"), &inst.rows, None);
                let _ = wrap.append_child(&c);
                c
            })
            .collect();
        let instances: Rc<Vec<ChartTypeSettingsInstance>> = Rc::new(instances.to_vec());
        let (bag, put, dlg, tid) = (
            self.bag.clone(),
            self.put.clone(),
            self.dlg.clone(),
            type_id.to_string(),
        );
        let refresh: Rc<dyn Fn()> = {
            let (cx, strip, contents) = (cx.clone(), strip.clone(), contents);
            let me: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
            let me2 = me.clone();
            let f: Rc<dyn Fn()> = Rc::new(move || {
                let present = |i: &ChartTypeSettingsInstance| {
                    i.enable_key
                        .as_ref()
                        .is_none_or(|k| bag.borrow().get(k).and_then(Value::as_bool) == Some(true))
                };
                let mut active = dlg.inst_active.borrow().get(&tid).copied().unwrap_or(0);
                if !instances.get(active).is_some_and(&present) {
                    active = 0;
                }
                dlg.inst_active.borrow_mut().insert(tid.clone(), active);
                clear_children(&strip);
                for (i, inst) in instances.iter().enumerate() {
                    if !present(inst) {
                        continue;
                    }
                    let tab = cx.el("button", "vela-sd-itab");
                    attr(&tab, "type", "button");
                    attr(&tab, "role", "tab");
                    attr(
                        &tab,
                        "aria-selected",
                        if i == active { "true" } else { "false" },
                    );
                    let _ = tab.append_child(&cx.text("span", "", &cx.tr.m(&inst.label)));
                    {
                        let (dlg, tid, me) = (dlg.clone(), tid.clone(), me2.clone());
                        cx.on(&tab, "click", move |_| {
                            dlg.inst_active.borrow_mut().insert(tid.clone(), i);
                            if let Some(f) = me.borrow().as_ref() {
                                f();
                            }
                        });
                    }
                    if i == active
                        && let Some(key) = inst.enable_key.clone()
                    {
                        let name = cx.tr.m(&inst.label);
                        let x = cx.text("button", "vela-sd-ix", "\u{2715}");
                        attr(&x, "type", "button");
                        let label = cx.tr.mv(
                            &msg("chart.settings.removeInstance", "Remove {name}"),
                            &[("name", name.clone().into())],
                        );
                        attr(&x, "title", &label);
                        attr(&x, "aria-label", &label);
                        let (dlg, tid, put) = (dlg.clone(), tid.clone(), put.clone());
                        cx.on(&x, "click", move |ev| {
                            crate::dom::stop_propagation(&ev);
                            dlg.inst_active.borrow_mut().insert(tid.clone(), 0);
                            put(&key, json!(false));
                        });
                        let _ = tab.append_child(&x);
                    }
                    let _ = strip.append_child(&tab);
                }
                if let Some(absent) = instances.iter().position(|i| !present(i)) {
                    let add = cx.text("button", "vela-sd-itab vela-sd-itab-add", "+");
                    attr(&add, "type", "button");
                    let name = cx.tr.m(&instances[absent].label);
                    let label = cx.tr.mv(
                        &msg("chart.settings.addInstance", "Add {name}"),
                        &[("name", name.into())],
                    );
                    attr(&add, "title", &label);
                    attr(&add, "aria-label", &label);
                    let (dlg, tid, put) = (dlg.clone(), tid.clone(), put.clone());
                    let key = instances[absent].enable_key.clone();
                    cx.on(&add, "click", move |_| {
                        dlg.inst_active.borrow_mut().insert(tid.clone(), absent);
                        if let Some(k) = &key {
                            put(k, json!(true));
                        }
                    });
                    let _ = strip.append_child(&add);
                }
                for (i, c) in contents.iter().enumerate() {
                    toggle_class(c, "vela-sd-hide", i != active);
                }
            });
            *me.borrow_mut() = Some(f.clone());
            f
        };
        self.add_refresher(refresh);
        wrap
    }

    fn grouped_rows(
        &self,
        pane_key: &str,
        rows: &[SettingsRowDescriptor],
        enable_key: Option<String>,
    ) -> Element {
        let cx = self.cx.clone();
        let wrap = cx.el("div", "vela-sd-struct");
        let toc = cx.el("div", "vela-sd-toc");
        let host = cx.el("div", "vela-sd-grid");
        attr(&host, "data-sd-rows", "1");
        let _ = wrap.append_child(&toc);
        let _ = wrap.append_child(&host);
        let wrap_ret = wrap.clone();

        struct Entry {
            el: Element,
            when: Option<SettingsRowWhen>,
            group: i64,
            key: Option<String>,
            header: bool,
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut groups: Vec<(String, MessageLike, Option<SettingsRowWhen>)> = Vec::new();
        let mut g: i64 = -1;
        for r in rows {
            match r {
                SettingsRowDescriptor::Heading { label, when } => {
                    g = groups.len() as i64;
                    groups.push((source_text(label), label.clone(), when.clone()));
                }
                SettingsRowDescriptor::Header { label, when } => {
                    let el = title_el(&cx, &cx.tr.m(label));
                    let _ = host.append_child(&el);
                    entries.push(Entry {
                        el,
                        when: when.clone(),
                        group: g,
                        key: None,
                        header: true,
                    });
                }
                other => {
                    let el = self.type_row(other);
                    let _ = host.append_child(&el);
                    let key = match other {
                        SettingsRowDescriptor::Row { toggle, .. } => {
                            toggle.as_ref().map(|t| t.key.clone())
                        }
                        SettingsRowDescriptor::Range { .. } => None,
                        SettingsRowDescriptor::Toggle { key, .. }
                        | SettingsRowDescriptor::Number { key, .. }
                        | SettingsRowDescriptor::Color { key, .. }
                        | SettingsRowDescriptor::Select { key, .. } => Some(key.clone()),
                        _ => None,
                    };
                    entries.push(Entry {
                        el,
                        when: row_when(other),
                        group: g,
                        key,
                        header: false,
                    });
                }
            }
        }
        let bag = self.bag.clone();
        let (dlg, pane_key) = (self.dlg.clone(), pane_key.to_string());
        let me: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let me2 = me.clone();
        let f: Rc<dyn Fn()> = Rc::new(move || {
            let live: Vec<bool> = (0..groups.len())
                .map(|gi| {
                    when_visible(&groups[gi].2, &bag)
                        && entries.iter().any(|e| {
                            e.group == gi as i64 && !e.header && when_visible(&e.when, &bag)
                        })
                })
                .collect();
            let remembered = dlg
                .group_active
                .borrow()
                .get(&pane_key)
                .cloned()
                .unwrap_or_default();
            let mut active = groups
                .iter()
                .position(|g| g.0 == remembered)
                .map_or(-1, |i| i as i64);
            if active < 0 || !live[active as usize] {
                active = live.iter().position(|l| *l).map_or(-1, |i| i as i64);
            }
            if active >= 0 {
                dlg.group_active
                    .borrow_mut()
                    .insert(pane_key.clone(), groups[active as usize].0.clone());
            }
            clear_children(&toc);
            for (gi, (ident, label, _)) in groups.iter().enumerate() {
                if !live[gi] {
                    continue;
                }
                let b = cx.text("button", "vela-sd-toc-btn", &cx.tr.m(label));
                attr(&b, "type", "button");
                attr(
                    &b,
                    "aria-pressed",
                    if gi as i64 == active { "true" } else { "false" },
                );
                let (dlg, pk, ident, me) =
                    (dlg.clone(), pane_key.clone(), ident.clone(), me2.clone());
                cx.on(&b, "click", move |_| {
                    dlg.group_active
                        .borrow_mut()
                        .insert(pk.clone(), ident.clone());
                    if let Some(f) = me.borrow().as_ref() {
                        f();
                    }
                });
                let _ = toc.append_child(&b);
            }
            let any = live.iter().any(|l| *l);
            toggle_class(&toc, "vela-sd-hide", !any);
            toggle_class(&wrap, "no-toc", !any);
            let enabled = enable_key
                .as_ref()
                .is_none_or(|k| bag.borrow().get(k).and_then(Value::as_bool) == Some(true));
            for i in 0..entries.len() {
                let e = &entries[i];
                let mut visible =
                    (e.group == -1 || e.group == active) && when_visible(&e.when, &bag);
                if visible && e.header {
                    let mut has = false;
                    for n in &entries[i + 1..] {
                        if n.group != e.group || n.header {
                            break;
                        }
                        if when_visible(&n.when, &bag) {
                            has = true;
                            break;
                        }
                    }
                    visible = has;
                }
                toggle_class(&e.el, "vela-sd-hide", !visible);
                toggle_class(
                    &e.el,
                    "vela-sd-soft",
                    visible && !enabled && e.key.as_ref() != enable_key.as_ref(),
                );
            }
        });
        *me.borrow_mut() = Some(f.clone());
        self.add_refresher(f);
        wrap_ret
    }
}

fn row_when(r: &SettingsRowDescriptor) -> Option<SettingsRowWhen> {
    match r {
        SettingsRowDescriptor::Heading { when, .. }
        | SettingsRowDescriptor::Header { when, .. }
        | SettingsRowDescriptor::Row { when, .. }
        | SettingsRowDescriptor::Toggle { when, .. }
        | SettingsRowDescriptor::Number { when, .. }
        | SettingsRowDescriptor::Color { when, .. }
        | SettingsRowDescriptor::Select { when, .. }
        | SettingsRowDescriptor::Range { when, .. } => when.clone(),
    }
}

/// The placement of a host section.
trait HostPlacement {
    fn placement_of(&self) -> &str;
}

impl HostPlacement for vela_core::ports::chart_renderer::SettingsSection {
    fn placement_of(&self) -> &str {
        // The section carries no placement field across the seam: read it from a row of
        // kind "placement" if the host sent one, else the original's default.
        self.rows
            .iter()
            .find(|r| r.get("kind").and_then(Value::as_str) == Some("placement"))
            .and_then(|r| r.get("value").and_then(Value::as_str))
            .unwrap_or("after-symbol")
    }
}

#[cfg(test)]
mod tests {
    use vela_core::render::chrome::settings_visibility::BUILTIN_SETTINGS_IDS;

    /// The id literals the dialog stamps (everything quoted that starts with a tab id).
    fn stamped() -> Vec<String> {
        let src = include_str!("settings.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let tabs = ["symbol", "scales", "canvas"].map(|t| format!("{t}."));
        let mut out: Vec<String> = Vec::new();
        for (i, chunk) in body.split('"').enumerate() {
            // Odd chunks sit between quotes.
            if i % 2 == 1
                && tabs.iter().any(|t| chunk.starts_with(t.as_str()))
                && !chunk.contains(' ')
                && !chunk.contains('{')
            {
                out.push(chunk.to_string());
            }
        }
        out
    }

    // `BUILTIN_SETTINGS_IDS` documents the `sid(...)` literals of the dialog ("keep the two
    // in sync"): the catalog drives `listSettingsIds`, so a drift either way would make the
    // visibility policy address ids the dialog never stamps (or hide nothing).
    #[test]
    fn every_catalogued_builtin_id_is_stamped_by_the_dialog() {
        let src = include_str!("settings.rs");
        for id in BUILTIN_SETTINGS_IDS {
            assert!(
                src.contains(&format!("\"{id}\"")),
                "builtin settings id {id} is not stamped by the dialog"
            );
        }
    }

    #[test]
    fn every_id_the_dialog_stamps_is_catalogued() {
        let catalog: std::collections::HashSet<&str> =
            BUILTIN_SETTINGS_IDS.iter().copied().collect();
        for id in stamped() {
            assert!(
                catalog.contains(id.as_str()),
                "the dialog stamps {id}, which BUILTIN_SETTINGS_IDS does not list"
            );
        }
    }
}
