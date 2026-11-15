//! The per-indicator settings dialog: a port of `IndicatorInputsDialog.ts`.
//!
//! A tabbed live-edit form built from the indicator's input schema (tabs by `tab=`, sections
//! by `group=`, `inline=` rows, `when` gates, tooltips, group help) plus a trailing
//! Properties tab for declaration props. Every edit is committed live through
//! `renderer.notify_input_change`. Cancel reverts to the open-time snapshot, OK, the close
//! button, the scrim and Escape keep the live edits, Reset defaults restores each input's
//! declared default (still cancelable).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::i18n::MessageLike;
use vela_core::model::{
    InputSchema, InputType, InputValue, Inputs, input_visible, join_multi_value, parse_multi_value,
};
use vela_core::ports::chart_renderer::{InputChangeEvent, InputChangeKind};
use vela_core::render::native::NativeRenderer;
use wasm_bindgen::JsCast;
use web_sys::Element;

use super::controls::{self, SelOpt};
use super::inputs::*;
use super::js::*;
use super::{Cx, Dialogs, Frame, Listener, clear_children, set_flag_attr};
use crate::dom::{attr, set_style};

/// The indicator being edited: its schema and the values of this session.
struct Row {
    id: String,
    inputs: Vec<InputSchema>,
    values: Inputs,
    props: Vec<InputSchema>,
    prop_values: Inputs,
}

struct Gate {
    el: Element,
    visible: Box<dyn Fn(&Inputs) -> bool>,
}

struct Sess {
    cx: Cx,
    frame: Frame,
    row: Rc<RefCell<Row>>,
    snapshot: RefCell<Inputs>,
    prop_snapshot: RefCell<Inputs>,
    gates: RefCell<Vec<Gate>>,
    tab_gates: RefCell<Vec<(usize, Box<dyn Fn(&Inputs) -> bool>)>>,
    tabs: RefCell<Vec<(Element, Element)>>,
    active: Cell<usize>,
    /// Closes the open multi-select list, if one is open.
    popup: RefCell<Option<Rc<dyn Fn()>>>,
    prev_focus: Option<Element>,
    renderer: Rc<NativeRenderer>,
}

/// The indicator settings dialog of one chart.
pub struct IndicatorDialog {
    d: Weak<Dialogs>,
    me: RefCell<Weak<IndicatorDialog>>,
    session: RefCell<Option<Rc<Sess>>>,
}

impl IndicatorDialog {
    pub fn new(d: Weak<Dialogs>) -> Rc<Self> {
        let s = Rc::new(Self {
            d,
            me: RefCell::new(Weak::new()),
            session: RefCell::new(None),
        });
        *s.me.borrow_mut() = Rc::downgrade(&s);
        s
    }

    pub fn open_id(&self) -> Option<String> {
        self.session
            .borrow()
            .as_ref()
            .map(|s| s.row.borrow().id.clone())
    }

    /// Open the dialog of mounted indicator `id` (nothing for an unknown id, or one with no
    /// inputs and no props).
    pub fn open_for(self: &Rc<Self>, id: &str) {
        let Some(d) = self.d.upgrade() else { return };
        let Some(renderer) = d.renderer() else { return };
        let Some(model) = renderer.indicator_model(id) else {
            return;
        };
        let (values, prop_values) = d
            .values_source
            .borrow()
            .as_ref()
            .and_then(|f| f(id))
            .unwrap_or_else(|| (model.input_values.clone(), model.prop_values.clone()));
        // The title is the compact text the legend chip shows.
        let title = renderer
            .legend_title_of(id)
            .unwrap_or_else(|| model.title.clone());
        let row = Row {
            id: id.to_string(),
            inputs: model.inputs.clone(),
            values,
            props: model.props.clone(),
            prop_values,
        };
        self.build(row, &title, None, d.active_element(), renderer);
    }

    pub fn close(&self) {
        self.close_inner(true);
    }

    pub fn destroy(&self) {
        self.close_inner(false);
    }

    fn close_inner(&self, restore: bool) {
        let session = self.session.borrow_mut().take();
        let Some(s) = session else { return };
        let taken_1 = s.popup.borrow_mut().take();
        if let Some(close) = taken_1 {
            close();
        }
        s.frame.positioner.remove();
        if let Some(d) = self.d.upgrade() {
            d.retire(s.cx.bag.clone());
            if restore {
                d.restore_focus(s.prev_focus.clone());
            }
        }
    }

    fn notify(s: &Sess, key: &str, value: InputValue, prop: bool) {
        let id = s.row.borrow().id.clone();
        s.renderer.notify_input_change(&InputChangeEvent {
            indicator_id: id,
            key: key.to_string(),
            value,
            kind: prop.then_some(InputChangeKind::Prop),
        });
    }

    fn build(
        self: &Rc<Self>,
        row: Row,
        title: &str,
        snapshots: Option<(Inputs, Inputs)>,
        prev_focus: Option<Element>,
        renderer: Rc<NativeRenderer>,
    ) {
        self.close_inner(false);
        if row.inputs.is_empty() && row.props.is_empty() {
            return;
        }
        let Some(d) = self.d.upgrade() else { return };
        let tr = d.tr();
        let cx = Cx::new(&d.doc, tr.clone());
        let mobile = d.mobile();
        let weak = self.me.borrow().clone();
        let title = title.to_string();
        let frame = Frame::build(&cx, "form", true, &title, mobile, {
            let w = weak.clone();
            move || {
                if let Some(s) = w.upgrade() {
                    s.close();
                }
            }
        });
        let (snapshot, prop_snapshot) =
            snapshots.unwrap_or_else(|| (row.values.clone(), row.prop_values.clone()));
        let row = Rc::new(RefCell::new(row));
        let s = Rc::new(Sess {
            cx: cx.clone(),
            frame,
            row: row.clone(),
            snapshot: RefCell::new(snapshot),
            prop_snapshot: RefCell::new(prop_snapshot),
            gates: RefCell::new(Vec::new()),
            tab_gates: RefCell::new(Vec::new()),
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(0),
            popup: RefCell::new(None),
            prev_focus,
            renderer,
        });
        let sweak = Rc::downgrade(&s);

        // Footer: Reset defaults (left), Cancel, OK.
        {
            let (w, sw, title2) = (weak.clone(), sweak.clone(), title.clone());
            let b = controls::button(
                &cx,
                &tr.t("chart.indicatorSettings.resetDefaults", "Reset defaults"),
                false,
                true,
                move || {
                    if let (Some(me), Some(s)) = (w.upgrade(), sw.upgrade()) {
                        me.reset(&s, &title2);
                    }
                },
            );
            let _ = s.frame.footer.append_child(&b);
            let (w, sw) = (weak.clone(), sweak.clone());
            let b = controls::button(
                &cx,
                &tr.t("chart.indicatorSettings.cancel", "Cancel"),
                false,
                false,
                move || {
                    if let (Some(me), Some(s)) = (w.upgrade(), sw.upgrade()) {
                        me.revert_and_close(&s);
                    }
                },
            );
            let _ = s.frame.footer.append_child(&b);
            let w = weak.clone();
            let b = controls::button(
                &cx,
                &tr.t("chart.indicatorSettings.ok", "OK"),
                true,
                false,
                move || {
                    if let Some(me) = w.upgrade() {
                        me.close();
                    }
                },
            );
            let _ = s.frame.footer.append_child(&b);
        }
        // Scrim press closes (an open list takes the first press); Escape likewise.
        {
            let (w, sw, pos) = (weak.clone(), sweak.clone(), s.frame.positioner.clone());
            cx.on(&s.frame.positioner, "pointerdown", move |ev| {
                if !is_el(&dlg_target(&ev), &pos) {
                    return;
                }
                if let Some(s) = sw.upgrade()
                    && let Some(close) = s.popup.borrow_mut().take()
                {
                    close();
                    return;
                }
                if let Some(me) = w.upgrade() {
                    me.close();
                    if let Some(d) = me.d.upgrade() {
                        d.backdrop_closed();
                    }
                }
            });
            let (w, sw) = (weak.clone(), sweak.clone());
            let l = Listener::new(&d.doc, "keydown", false, false, move |ev| {
                if dlg_key(&ev) != "Escape" {
                    return;
                }
                crate::dom::prevent_default(&ev);
                if let Some(s) = sw.upgrade()
                    && let Some(close) = s.popup.borrow_mut().take()
                {
                    close();
                    return;
                }
                if let Some(me) = w.upgrade() {
                    me.close();
                }
            });
            cx.bag.borrow_mut().push(l);
        }

        // Tabs.
        let (inputs, props) = {
            let r = row.borrow();
            (r.inputs.clone(), r.props.clone())
        };
        let mut tab_defs = tab_inputs(&inputs);
        if !props.is_empty() {
            let decls: Vec<Decl> = props
                .iter()
                .map(|p| {
                    let mut schema = p.clone();
                    schema.tab = None;
                    Decl { schema, prop: true }
                })
                .collect();
            match tab_defs.iter_mut().find(|t| t.name == PROPS_TAB) {
                Some(t) => t.inputs.extend(decls),
                None => tab_defs.push(InputTab {
                    name: PROPS_TAB.to_string(),
                    inputs: decls,
                }),
            }
        }
        let strip = cx.el("div", "vela-ind-tabs");
        attr(&strip, "role", "tablist");
        if tab_defs.len() == 1 {
            attr(&strip, "data-single", "");
        }
        let _ = s.frame.body.append_child(&strip);
        for (idx, def) in tab_defs.iter().enumerate() {
            let label = match def.name.as_str() {
                DEFAULT_INPUT_TAB => tr.t("chart.indicatorSettings.inputsTab", "Inputs"),
                PROPS_TAB => tr.t("chart.indicatorSettings.propertiesTab", "Properties"),
                other => other.to_string(),
            };
            let tab = cx.text("button", "vela-ind-tab", &label);
            attr(&tab, "type", "button");
            attr(&tab, "role", "tab");
            let (tid, pid) = (cx.uid("vela-ind-tab"), cx.uid("vela-ind-panel"));
            tab.set_id(&tid);
            attr(&tab, "aria-controls", &pid);
            let body = cx.el("div", "vela-ind-body");
            body.set_id(&pid);
            attr(&body, "role", "tabpanel");
            attr(&body, "aria-labelledby", &tid);
            self.fill_tab(&s, &body, &def.inputs);
            if def.inputs.iter().all(|d| !d.schema.when.is_empty()) {
                let decls = def.inputs.clone();
                s.tab_gates.borrow_mut().push((
                    idx,
                    Box::new(move |bag: &Inputs| {
                        decls.iter().any(|d| input_visible(&d.schema.when, bag))
                    }),
                ));
            }
            {
                let sw = sweak.clone();
                cx.on(&tab, "click", move |_| {
                    if let Some(s) = sw.upgrade() {
                        show_tab(&s, idx);
                    }
                });
                let sw = sweak.clone();
                cx.on(&tab, "keydown", move |ev| {
                    let Some(s) = sw.upgrade() else { return };
                    let n = s.tabs.borrow().len();
                    let cur = s.active.get();
                    let next = match dlg_key(&ev).as_str() {
                        "ArrowRight" => Some((cur + 1) % n),
                        "ArrowLeft" => Some((cur + n - 1) % n),
                        "Home" => Some(0),
                        "End" => Some(n - 1),
                        _ => None,
                    };
                    if let Some(next) = next {
                        crate::dom::prevent_default(&ev);
                        show_tab(&s, next);
                        let b = s.tabs.borrow()[next].0.clone();
                        crate::dom::vela_focus(&b);
                    }
                });
            }
            let _ = strip.append_child(&tab);
            let _ = s.frame.body.append_child(&body);
            s.tabs.borrow_mut().push((tab, body));
        }
        show_tab(&s, 0);
        refresh_gates(&s);

        d.mount_frame(&s.frame.positioner);
        *self.session.borrow_mut() = Some(s.clone());
        // Focus the first control.
        let first = s.tabs.borrow().first().map(|t| t.0.clone());
        if let Some(f) = first {
            crate::dom::vela_focus(&f);
        }
    }

    /// Restore every input and prop to its open-time value (notifying the host), then close.
    fn revert_and_close(&self, s: &Rc<Sess>) {
        let (inputs, props) = {
            let r = s.row.borrow();
            (r.inputs.clone(), r.props.clone())
        };
        let snap = s.snapshot.borrow().clone();
        for inp in &inputs {
            let before = snap
                .get(&inp.key)
                .cloned()
                .unwrap_or_else(|| inp.defval.clone());
            let changed = s.row.borrow().values.get(&inp.key) != Some(&before);
            if changed {
                s.row
                    .borrow_mut()
                    .values
                    .insert(inp.key.clone(), before.clone());
                Self::notify(s, &inp.key, before, false);
            }
        }
        let psnap = s.prop_snapshot.borrow().clone();
        for p in &props {
            let before = psnap
                .get(&p.key)
                .cloned()
                .unwrap_or_else(|| p.defval.clone());
            let changed = s.row.borrow().prop_values.get(&p.key) != Some(&before);
            if changed {
                s.row
                    .borrow_mut()
                    .prop_values
                    .insert(p.key.clone(), before.clone());
                Self::notify(s, &p.key, before, true);
            }
        }
        self.close();
    }

    /// Restore every input to its declared default, then rebuild the form so each control
    /// re-reads the values. The open-time snapshot survives, so Cancel still reverts it all.
    fn reset(self: &Rc<Self>, s: &Rc<Sess>, title: &str) {
        let (inputs, props) = {
            let r = s.row.borrow();
            (r.inputs.clone(), r.props.clone())
        };
        for inp in &inputs {
            if s.row.borrow().values.get(&inp.key) != Some(&inp.defval) {
                s.row
                    .borrow_mut()
                    .values
                    .insert(inp.key.clone(), inp.defval.clone());
                Self::notify(s, &inp.key, inp.defval.clone(), false);
            }
        }
        for p in &props {
            if s.row.borrow().prop_values.get(&p.key) != Some(&p.defval) {
                s.row
                    .borrow_mut()
                    .prop_values
                    .insert(p.key.clone(), p.defval.clone());
                Self::notify(s, &p.key, p.defval.clone(), true);
            }
        }
        let row = {
            let r = s.row.borrow();
            Row {
                id: r.id.clone(),
                inputs: r.inputs.clone(),
                values: r.values.clone(),
                props: r.props.clone(),
                prop_values: r.prop_values.clone(),
            }
        };
        let snaps = Some((
            s.snapshot.borrow().clone(),
            s.prop_snapshot.borrow().clone(),
        ));
        let (prev, renderer) = (s.prev_focus.clone(), s.renderer.clone());
        self.build(row, title, snaps, prev, renderer);
    }

    // ── one tab ──

    fn fill_tab(self: &Rc<Self>, s: &Rc<Sess>, body: &Element, inputs: &[Decl]) {
        let cx = &s.cx;
        for group in group_inputs(inputs) {
            let section = cx.el("div", "vela-ind-grp");
            let mut header: Option<Element> = None;
            if let Some(name) = &group.name {
                let text = group.label.as_ref().map_or(name.clone(), |m| cx.tr.m(m));
                let h = cx.text("div", "vela-ind-section", &text);
                attr(&h, "role", "heading");
                attr(&h, "aria-level", "3");
                if let Some(help) = &group.help {
                    let t = help.text.as_ref().map(|m| cx.tr.m(m)).unwrap_or_default();
                    let href = help
                        .url
                        .as_deref()
                        .and_then(|u| controls::safe_url(u, true));
                    if !t.is_empty() || href.is_some() {
                        let mark = controls::hint(
                            cx,
                            &t,
                            href.as_deref(),
                            &cx.tr.t("chart.indicatorSettings.help", "Help"),
                            true,
                        );
                        let _ = h.append_child(&mark);
                    }
                }
                let _ = section.append_child(&h);
                header = Some(h);
            }
            for r in &group.rows {
                let el = self.input_row(s, r);
                let _ = section.append_child(&el);
                if r.iter().any(|d| !d.schema.when.is_empty()) {
                    let decls = r.clone();
                    s.gates.borrow_mut().push(Gate {
                        el,
                        visible: Box::new(move |bag| row_visible(&decls, bag)),
                    });
                }
            }
            if let Some(h) = header
                && group
                    .rows
                    .iter()
                    .all(|r| r.iter().all(|d| !d.schema.when.is_empty()))
            {
                let rows = group.rows.clone();
                s.gates.borrow_mut().push(Gate {
                    el: h,
                    visible: Box::new(move |bag| rows.iter().any(|r| row_visible(r, bag))),
                });
            }
            let _ = body.append_child(&section);
        }
    }

    fn input_row(self: &Rc<Self>, s: &Rc<Sess>, decls: &[Decl]) -> Element {
        let cx = &s.cx;
        let rid = s.row.borrow().id.clone();
        let idof = |d: &Decl| format!("vela-inp-{rid}-{}", d.schema.key);
        // The hint exists only for an input that declares a `tooltip` (its translated
        // `display.tooltip` then takes over the text), as in the original.
        let info = |d: &Decl| -> Option<Element> {
            d.schema.tooltip.as_deref().filter(|t| !t.is_empty())?;
            let t = tooltip_of(&d.schema, &cx.tr);
            Some(controls::hint(cx, &t, None, &t, false))
        };
        let row = cx.el("div", "vela-ind-row");
        if decls.len() > 1 {
            let wrap = cx.el("div", "vela-ind-inline");
            for d in decls {
                let id = idof(d);
                let item = cx.el("div", "vela-ind-inline-item");
                let name = name_of(&d.schema, &cx.tr);
                let ctl = self.control(s, d, &id);
                if d.schema.input_type == InputType::Bool {
                    let _ = item.append_child(&ctl);
                    if !name.is_empty() {
                        let l = cx.text("label", "", &name);
                        attr(&l, "for", &id);
                        let _ = item.append_child(&l);
                    }
                } else {
                    if !name.is_empty() {
                        let l = cx.text("label", "", &name);
                        attr(&l, "for", &id);
                        let _ = item.append_child(&l);
                    }
                    let _ = item.append_child(&ctl);
                }
                let _ = wrap.append_child(&item);
            }
            if let Some(i) = decls.iter().rev().find_map(&info) {
                let _ = wrap.append_child(&i);
            }
            let _ = row.append_child(&wrap);
            return row;
        }
        let d = &decls[0];
        let id = idof(d);
        let name = name_of(&d.schema, &cx.tr);
        let ctl = self.control(s, d, &id);
        match d.schema.input_type {
            InputType::Bool => {
                let w = cx.el("div", "vela-ind-bool");
                let _ = w.append_child(&ctl);
                let l = cx.text("label", "", &name);
                attr(&l, "for", &id);
                let _ = w.append_child(&l);
                if let Some(i) = info(d) {
                    let _ = w.append_child(&i);
                }
                let _ = row.append_child(&w);
            }
            InputType::TextArea => {
                let w = cx.el("div", "vela-ind-stacked");
                let head = cx.el("div", "vela-ind-label");
                let l = cx.text("label", "", &name);
                attr(&l, "for", &id);
                let _ = head.append_child(&l);
                if let Some(i) = info(d) {
                    let _ = head.append_child(&i);
                }
                let _ = w.append_child(&head);
                let _ = w.append_child(&ctl);
                let _ = row.append_child(&w);
            }
            _ => {
                let head = cx.el("div", "vela-ind-label");
                let l = cx.text("label", "", &name);
                attr(&l, "for", &id);
                let _ = head.append_child(&l);
                if let Some(i) = info(d) {
                    let _ = head.append_child(&i);
                }
                let cell = cx.el("div", "vela-ind-cell");
                if is_fit_control(&d.schema) {
                    attr(&cell, "data-fit", "");
                }
                let _ = cell.append_child(&ctl);
                let _ = row.append_child(&head);
                let _ = row.append_child(&cell);
            }
        }
        row
    }

    /// Write one edit through: store it, notify the host, re-apply the `when` gates.
    fn commit(s: &Rc<Sess>, d: &Decl, value: InputValue) {
        {
            let mut r = s.row.borrow_mut();
            let bag = if d.prop {
                &mut r.prop_values
            } else {
                &mut r.values
            };
            bag.insert(d.schema.key.clone(), value.clone());
        }
        Self::notify(s, &d.schema.key, value, d.prop);
        refresh_gates(s);
    }

    /// The typed control of one input.
    fn control(self: &Rc<Self>, s: &Rc<Sess>, d: &Decl, id: &str) -> Element {
        let cx = s.cx.clone();
        let inp = d.schema.clone();
        let (sw, dd) = (Rc::downgrade(s), d.clone());
        let current: Rc<dyn Fn() -> InputValue> = {
            let (sw, dd) = (sw.clone(), dd.clone());
            Rc::new(move || {
                sw.upgrade()
                    .map(|s| {
                        let r = s.row.borrow();
                        let bag = if dd.prop { &r.prop_values } else { &r.values };
                        current_of(bag, &dd.schema).clone()
                    })
                    .unwrap_or_else(|| dd.schema.defval.clone())
            })
        };
        let emit: Rc<dyn Fn(InputValue)> = {
            let (sw, dd) = (sw.clone(), dd.clone());
            Rc::new(move |v| {
                if let Some(s) = sw.upgrade() {
                    Self::commit(&s, &dd, v);
                }
            })
        };
        let aria = name_of(&inp, &cx.tr);
        let text_get = {
            let c = current.clone();
            Rc::new(move || c().to_text()) as Rc<dyn Fn() -> String>
        };
        let text_emit = {
            let e = emit.clone();
            Rc::new(move |v: String| e(InputValue::Text(v))) as Rc<dyn Fn(String)>
        };
        if inp.input_type == InputType::Bool {
            let (c, e) = (current.clone(), emit.clone());
            return controls::checkbox(
                &cx,
                id,
                &aria,
                Rc::new(move || bool_of(&c())),
                Rc::new(move |v| e(InputValue::Bool(v))),
            )
            .el;
        }
        if let Some(options) = inp.options.as_ref().filter(|o| !o.is_empty()) {
            let labels = inp.display.as_ref().and_then(|d| d.options.as_ref());
            let pairs: Vec<SelOpt> = options
                .iter()
                .map(|o| {
                    let label = labels
                        .and_then(|l| l.get(o))
                        .map_or(o.clone(), |m| cx.tr.m(m));
                    SelOpt::new(o.clone(), label)
                })
                .collect();
            if inp.multiple.is_some() && inp.input_type == InputType::String {
                return self.multi_select(s, &cx, id, &inp, &pairs, current, emit);
            }
            return controls::select(&cx, id, &aria, &pairs, text_get, text_emit).el;
        }
        match inp.input_type {
            InputType::Source => {
                // The engine may hold the source capitalized (`Close`): the list shows the
                // matching lowercase choice and an edit commits the lowercase one.
                let c = current.clone();
                let text_get: Rc<dyn Fn() -> String> = Rc::new(move || {
                    let v = c().to_text();
                    let lower = v.to_lowercase();
                    if SOURCES.contains(&lower.as_str()) {
                        lower
                    } else {
                        v
                    }
                });
                let opts: Vec<SelOpt> = SOURCES.iter().map(|o| SelOpt::new(*o, *o)).collect();
                controls::select(&cx, id, &aria, &opts, text_get, text_emit).el
            }
            InputType::Color => controls::color_field(&cx, id, &aria, true, text_get, text_emit).el,
            InputType::Symbol => self.symbol(s, &cx, id, &aria, current, emit),
            InputType::Timeframe => {
                let opts: Vec<SelOpt> = timeframe_options()
                    .into_iter()
                    .map(|(v, l): (&str, MessageLike)| SelOpt::new(v, cx.tr.m(&l)))
                    .collect();
                controls::select(&cx, id, &aria, &opts, text_get, text_emit).el
            }
            InputType::Session => session(&cx, id, current, emit),
            InputType::Time => time(&cx, id, current, emit),
            InputType::TextArea => controls::textarea(&cx, id, &aria, text_get, text_emit).el,
            InputType::Int | InputType::Float | InputType::Price => {
                let integer = inp.input_type == InputType::Int;
                let (c, e) = (current.clone(), emit.clone());
                controls::number(
                    &cx,
                    id,
                    controls::NumOpts {
                        min: inp.min,
                        max: inp.max,
                        step: Some(inp.step.unwrap_or(if integer { 1.0 } else { 0.1 })),
                        integer,
                        live: false,
                        aria,
                        ..controls::NumOpts::default()
                    },
                    Rc::new(move || number_of(&c())),
                    Rc::new(move |v| e(InputValue::Number(v))),
                )
                .el
            }
            _ => controls::text_field(&cx, id, &aria, text_get, text_emit).el,
        }
    }

    /// `input.symbol`: a field that opens the host's ticker picker when one is wired, else a
    /// plain text field.
    fn symbol(
        &self,
        s: &Rc<Sess>,
        cx: &Cx,
        id: &str,
        aria: &str,
        current: Rc<dyn Fn() -> InputValue>,
        emit: Rc<dyn Fn(InputValue)>,
    ) -> Element {
        let Some(picker) = s.renderer.symbol_picker() else {
            let (c, e) = (current.clone(), emit.clone());
            return controls::text_field(
                cx,
                id,
                aria,
                Rc::new(move || c().to_text()),
                Rc::new(move |v| e(InputValue::Text(v))),
            )
            .el;
        };
        let btn = cx.el("button", "vela-ctl-trigger");
        attr(&btn, "type", "button");
        btn.set_id(id);
        attr(&btn, "aria-label", aria);
        let value = Rc::new(RefCell::new(current().to_text()));
        let label = cx.el("span", "vela-ctl-trigger-value");
        let paint = {
            let (label, tr) = (label.clone(), cx.tr.clone());
            move |v: &str| {
                label.set_text_content(Some(&if v.is_empty() {
                    tr.t("chart.indicatorSettings.selectSymbol", "Select symbol…")
                } else {
                    v.to_string()
                }));
            }
        };
        paint(&value.borrow());
        let _ = btn.append_child(&label);
        let (label2, value2, tr) = (label.clone(), value.clone(), cx.tr.clone());
        cx.on(&btn, "click", move |_| {
            let (label3, value3, emit3, tr3) =
                (label2.clone(), value2.clone(), emit.clone(), tr.clone());
            let seed = value2.borrow().clone();
            picker(
                &seed,
                Box::new(move |picked: String| {
                    if picked.is_empty() {
                        return;
                    }
                    *value3.borrow_mut() = picked.clone();
                    label3.set_text_content(Some(&if picked.is_empty() {
                        tr3.t("chart.indicatorSettings.selectSymbol", "Select symbol…")
                    } else {
                        picked.clone()
                    }));
                    emit3(InputValue::Text(picked));
                }),
            );
        });
        btn
    }

    /// A multi-select string input: a trigger showing the checked options joined by the
    /// separator, opening a list of checkboxes that stays open while the user toggles. Locked
    /// options are checked and disabled. Escape, Tab, an outside press or the trigger closes it.
    #[allow(clippy::too_many_arguments)]
    fn multi_select(
        self: &Rc<Self>,
        s: &Rc<Sess>,
        cx: &Cx,
        id: &str,
        inp: &InputSchema,
        pairs: &[SelOpt],
        current: Rc<dyn Fn() -> InputValue>,
        emit: Rc<dyn Fn(InputValue)>,
    ) -> Element {
        let wrap = cx.el("div", "vela-ctl-multi");
        let btn = cx.el("button", "vela-ctl-trigger");
        attr(&btn, "type", "button");
        btn.set_id(id);
        attr(&btn, "aria-haspopup", "true");
        attr(&btn, "aria-expanded", "false");
        let text = cx.el("span", "vela-ctl-trigger-value");
        let chev = cx.el("span", "vela-ctl-trigger-chev");
        chev.set_inner_html(&super::icons::icon("chevron-down", 12));
        let _ = btn.append_child(&text);
        let _ = btn.append_child(&chev);
        let _ = wrap.append_child(&btn);
        let options = inp.options.clone();
        let multiple = inp.multiple.clone();
        let sep = multiple
            .as_ref()
            .map(|m| m.separator.clone())
            .unwrap_or_default();
        let locked: Vec<String> = multiple
            .as_ref()
            .map(|m| m.locked.clone())
            .unwrap_or_default();
        let checked = Rc::new(RefCell::new(parse_multi_value(
            options.as_deref(),
            multiple.as_ref(),
            &current(),
        )));
        let none = cx.tr.t("chart.indicatorSettings.noneChecked", "None");
        let paint = {
            let (text, checked, pairs, sep) =
                (text.clone(), checked.clone(), pairs.to_vec(), sep.clone());
            Rc::new(move || {
                let shown = checked
                    .borrow()
                    .iter()
                    .map(|v| {
                        pairs
                            .iter()
                            .find(|p| p.value == *v)
                            .map_or(v.clone(), |p| p.label.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(&sep);
                set_flag_attr(&text, "data-placeholder", shown.is_empty());
                text.set_text_content(Some(&if shown.is_empty() {
                    none.clone()
                } else {
                    shown
                }));
            })
        };
        paint();
        let toggle: Rc<dyn Fn(&str, bool)> = {
            let (checked, paint, options, multiple, emit) = (
                checked.clone(),
                paint.clone(),
                options.clone(),
                multiple.clone(),
                emit.clone(),
            );
            Rc::new(move |value, on| {
                let mut set: Vec<String> = checked.borrow().clone();
                set.retain(|v| v != value);
                if on {
                    set.push(value.to_string());
                }
                let joined = join_multi_value(
                    options.as_deref(),
                    multiple.as_ref(),
                    set.iter().map(String::as_str),
                );
                *checked.borrow_mut() = parse_multi_value(
                    options.as_deref(),
                    multiple.as_ref(),
                    &InputValue::Text(joined.clone()),
                );
                paint();
                emit(InputValue::Text(joined));
            })
        };
        let open_pop: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));
        let bag_slot: Rc<RefCell<Option<Rc<RefCell<Vec<Listener>>>>>> = Rc::new(RefCell::new(None));
        let close: Rc<dyn Fn()> = {
            let (open_pop, btn, sw, wd, bag_slot) = (
                open_pop.clone(),
                btn.clone(),
                Rc::downgrade(s),
                self.d.clone(),
                bag_slot.clone(),
            );
            Rc::new(move || {
                let taken_2 = open_pop.borrow_mut().take();
                if let Some(pop) = taken_2 {
                    pop.remove();
                    attr(&btn, "aria-expanded", "false");
                    let taken_3 = bag_slot.borrow_mut().take();
                    if let Some(bag) = taken_3
                        && let Some(d) = wd.upgrade()
                    {
                        d.retire(bag);
                    }
                    if let Some(s) = sw.upgrade() {
                        s.popup.borrow_mut().take();
                    }
                    crate::dom::vela_focus(&btn);
                }
            })
        };
        let open: Rc<dyn Fn()> = {
            let (cx, pairs, locked, checked, toggle, open_pop, close, btn, wrap, sw) = (
                cx.clone(),
                pairs.to_vec(),
                locked,
                checked.clone(),
                toggle,
                open_pop.clone(),
                close.clone(),
                btn.clone(),
                wrap.clone(),
                Rc::downgrade(s),
            );
            let id = id.to_string();
            Rc::new(move || {
                let pcx = cx.sub();
                let pop = pcx.el("div", "vela-ctl-pop");
                attr(&pop, "role", "group");
                let mut boxes: Vec<Element> = Vec::new();
                for (i, p) in pairs.iter().enumerate() {
                    let rid = format!("{id}-opt-{i}");
                    let row = pcx.el("div", "vela-ctl-pop-row");
                    let is_locked = locked.contains(&p.value);
                    if is_locked {
                        attr(&row, "data-locked", "");
                    }
                    let (value, toggle) = (p.value.clone(), toggle.clone());
                    let on = checked.borrow().contains(&p.value);
                    let boxc = controls::checkbox(
                        &pcx,
                        &rid,
                        "",
                        Rc::new(move || on),
                        Rc::new(move |v| toggle(&value, v)),
                    );
                    if is_locked {
                        attr(&boxc.el, "disabled", "");
                        dlg_set_checked(&boxc.el, true);
                    } else {
                        boxes.push(boxc.el.clone());
                    }
                    let l = pcx.text("label", "", &p.label);
                    attr(&l, "for", &rid);
                    // The row's own padding toggles too: the whole row is the target.
                    {
                        let (row2, input) = (row.clone(), boxc.el.clone());
                        pcx.on(&row, "click", move |ev| {
                            if is_el(&dlg_target(&ev), &row2) {
                                dlg_click(&input);
                            }
                        });
                    }
                    let _ = row.append_child(&boxc.el);
                    let _ = row.append_child(&l);
                    let _ = pop.append_child(&row);
                }
                // Keyboard inside the list.
                {
                    let (boxes, close) = (boxes.clone(), close.clone());
                    pcx.on(&pop, "keydown", move |ev| {
                        let key = dlg_key(&ev);
                        let target = dlg_target(&ev);
                        let at = boxes.iter().position(|b| is_el(&target, b));
                        let n = boxes.len() as i64;
                        if n == 0 {
                            return;
                        }
                        let go = |i: i64| {
                            crate::dom::prevent_default(&ev);
                            let b = &boxes[i.rem_euclid(n) as usize];
                            crate::dom::vela_focus(b);
                            dlg_scroll_into_view(b);
                        };
                        let cur = at.map_or(-1, |a| a as i64);
                        match key.as_str() {
                            "ArrowDown" => go(cur + 1),
                            "ArrowUp" => go(if cur < 0 { -1 } else { cur - 1 }),
                            "Home" => go(0),
                            "End" => go(-1),
                            "Enter" if at.is_some() => {
                                crate::dom::prevent_default(&ev);
                                dlg_click(&boxes[cur as usize]);
                            }
                            "Tab" => close(),
                            "Escape" => {
                                crate::dom::stop_propagation(&ev);
                                close();
                            }
                            _ => {}
                        }
                    });
                }
                // An outside press closes it.
                {
                    let (close, wrap) = (close.clone(), wrap.clone());
                    let l = Listener::new(&cx.doc, "pointerdown", true, false, move |ev| {
                        let t = dlg_target(&ev);
                        let inside = !t.is_null()
                            && !t.is_undefined()
                            && crate::dom::vela_contains(&wrap, &t);
                        if !inside {
                            close();
                        }
                    });
                    pcx.bag.borrow_mut().push(l);
                }
                let _ = wrap.append_child(&pop);
                attr(&btn, "aria-expanded", "true");
                *open_pop.borrow_mut() = Some(pop);
                *bag_slot.borrow_mut() = Some(pcx.bag.clone());
                if let Some(s) = sw.upgrade() {
                    *s.popup.borrow_mut() = Some(close.clone());
                }
                if let Some(b) = boxes.first() {
                    crate::dom::vela_focus(b);
                }
            })
        };
        {
            let (open1, close1, pop1) = (open.clone(), close.clone(), open_pop.clone());
            cx.on(&btn, "click", move |_| {
                if pop1.borrow().is_some() {
                    close1();
                } else {
                    open1();
                }
            });
            let (open, open_pop) = (open.clone(), open_pop.clone());
            cx.on(&btn, "keydown", move |ev| {
                let k = dlg_key(&ev);
                if (k == "ArrowDown" || k == "ArrowUp") && open_pop.borrow().is_none() {
                    crate::dom::prevent_default(&ev);
                    open();
                }
            });
        }
        wrap
    }
}

// ── session and time ──

fn time_input(cx: &Cx, id: &str, aria: &str, value: &str, on: Rc<dyn Fn(String)>) -> Element {
    let el = cx.el("input", "vela-ctl-text");
    attr(&el, "type", "time");
    attr(&el, "step", "1800");
    el.set_id(id);
    attr(&el, "aria-label", aria);
    set_style(&el, "width", "96px");
    dlg_set_value(&el, value);
    let (el2, last) = (el.clone(), Rc::new(RefCell::new(value.to_string())));
    cx.on(&el, "change", move |_| {
        match normalize_time_input(&dlg_value(&el2)) {
            Some(v) => {
                dlg_set_value(&el2, &v);
                if *last.borrow() != v {
                    *last.borrow_mut() = v.clone();
                    on(v);
                }
            }
            None => dlg_set_value(&el2, &last.borrow()),
        }
    });
    el
}

/// `input.session`: two HH:MM fields committing a `HHMM-HHMM` session string.
fn session(
    cx: &Cx,
    id: &str,
    current: Rc<dyn Fn() -> InputValue>,
    emit: Rc<dyn Fn(InputValue)>,
) -> Element {
    let (a, b) = session_to_times(&current().to_text());
    let wrap = cx.el("div", "vela-ctl-combo");
    let state = Rc::new(RefCell::new((a.clone(), b.clone())));
    let mk = |idx: usize, id: String, v: String, aria: String| {
        let (state, emit) = (state.clone(), emit.clone());
        time_input(
            cx,
            &id,
            &aria,
            &v,
            Rc::new(move |t| {
                {
                    let mut s = state.borrow_mut();
                    if idx == 0 {
                        s.0 = t;
                    } else {
                        s.1 = t;
                    }
                }
                let (x, y) = state.borrow().clone();
                emit(InputValue::Text(times_to_session(&x, &y)));
            }),
        )
    };
    let _ = wrap.append_child(&mk(
        0,
        id.to_string(),
        a,
        cx.tr
            .t("chart.indicatorSettings.sessionStart", "Session start"),
    ));
    let _ = wrap.append_child(&mk(
        1,
        format!("{id}-end"),
        b,
        cx.tr.t("chart.indicatorSettings.sessionEnd", "Session end"),
    ));
    wrap
}

/// `input.time`: a date field and a time field committing an epoch-ms timestamp.
fn time(
    cx: &Cx,
    id: &str,
    current: Rc<dyn Fn() -> InputValue>,
    emit: Rc<dyn Fn(InputValue)>,
) -> Element {
    let ts = number_of(&current());
    let (date, time) = if ts == 0.0 {
        (String::new(), "09:30".to_string())
    } else {
        let ms = if ts < 1e12 { ts * 1000.0 } else { ts };
        let p = dlg_date_parts(ms);
        if p.length() < 5 {
            (String::new(), "09:30".to_string())
        } else {
            let g = |i: u32| p.get(i).as_f64().unwrap_or(0.0) as i32;
            snap_parts(g(0), g(1) as u32, g(2) as u32, g(3) as u32, g(4) as u32)
        }
    };
    let state = Rc::new(RefCell::new((date.clone(), time.clone())));
    let commit = {
        let (state, emit) = (state.clone(), emit.clone());
        Rc::new(move || {
            let (d, t) = state.borrow().clone();
            if d.is_empty() {
                return;
            }
            if let Some((y, mo, dd, h, mi)) = parse_date_time(&d, &t) {
                let ms = dlg_date_ms(y, mo, dd, h, mi);
                if ms.is_finite() {
                    emit(InputValue::Number(ms));
                }
            }
        })
    };
    let wrap = cx.el("div", "vela-ctl-combo");
    let date_el = cx.el("input", "vela-ctl-text");
    attr(&date_el, "type", "date");
    date_el.set_id(id);
    attr(
        &date_el,
        "aria-label",
        &cx.tr.t("chart.indicatorSettings.date", "Date"),
    );
    set_style(&date_el, "width", "150px");
    dlg_set_value(&date_el, &date);
    {
        let (de, state, commit) = (date_el.clone(), state.clone(), commit.clone());
        cx.on(&date_el, "change", move |_| {
            state.borrow_mut().0 = dlg_value(&de);
            commit();
        });
    }
    let (state2, commit2) = (state.clone(), commit.clone());
    let time_el = time_input(
        cx,
        &format!("{id}-time"),
        &cx.tr.t("chart.indicatorSettings.time", "Time"),
        &time,
        Rc::new(move |t| {
            state2.borrow_mut().1 = t;
            commit2();
        }),
    );
    let _ = wrap.append_child(&date_el);
    let _ = wrap.append_child(&time_el);
    wrap
}

// ── tabs and gates ──

fn show_tab(s: &Rc<Sess>, idx: usize) {
    s.active.set(idx);
    for (i, (tab, body)) in s.tabs.borrow().iter().enumerate() {
        set_style(body, "display", if i == idx { "grid" } else { "none" });
        attr(
            tab,
            "aria-selected",
            if i == idx { "true" } else { "false" },
        );
        tab.unchecked_ref::<web_sys::HtmlElement>()
            .set_tab_index(if i == idx { 0 } else { -1 });
    }
}

/// Re-apply every `when` gate against the current values.
fn refresh_gates(s: &Rc<Sess>) {
    let bag = {
        // Gates read the script inputs only; declaration props never feed them.
        let r = s.row.borrow();
        values_bag(&r.inputs, &r.values)
    };
    for g in s.gates.borrow().iter() {
        set_style(
            &g.el,
            "display",
            if (g.visible)(&bag) { "" } else { "none" },
        );
    }
    let mut first_live = None;
    let mut active_hidden = false;
    for (i, (tab, _)) in s.tabs.borrow().iter().enumerate() {
        let gate = s.tab_gates.borrow();
        let visible = gate
            .iter()
            .find(|(idx, _)| *idx == i)
            .is_none_or(|(_, f)| f(&bag));
        set_style(tab, "display", if visible { "" } else { "none" });
        if visible && first_live.is_none() {
            first_live = Some(i);
        }
        if !visible && i == s.active.get() {
            active_hidden = true;
        }
    }
    if active_hidden && let Some(i) = first_live {
        show_tab(s, i);
    }
    let _ = clear_children;
}
