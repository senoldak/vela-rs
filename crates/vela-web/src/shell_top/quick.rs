//! The timeframe quick entry (`timeframe-quick.ts`: type a number to change the timeframe)
//! and the two mobile bottom sheets (`timeframe-drawer.ts`, `timezone-drawer.ts`).
//!
//! The logic is [`vela_core::widget::timeframe_quick`], [`timeframe_drawer`] and
//! [`timezone_drawer`]; these are their elements.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::widget::bottombar::RANGE_PRESETS;
use vela_core::widget::timeframe_drawer::timeframe_drawer_view;
use vela_core::widget::timeframe_quick::{
    extend_entry, seed_entry, timeframe_hint, timeframe_to_apply, title_message,
};
use vela_core::widget::timezone_drawer::{timezone_drawer_rows, title_message as tz_title};
use web_sys::Element;

use super::js::{vst_key, vst_set_value, vst_value};
use super::kit::{Dialog, DialogOpts, Drawer, Env, clear, icon_el};
use crate::dom::{Listener, attr, make, on, vela_caret_end, vela_focus};

// ── quick entry ──

pub struct TimeframeQuick {
    env: Env,
    me: Weak<TimeframeQuick>,
    dialog: Rc<Dialog>,
    input: Element,
    hint: Element,
    on_apply: Rc<dyn Fn(&str)>,
    perm: RefCell<Vec<Listener>>,
}

impl TimeframeQuick {
    pub fn new(
        env: &Env,
        on_apply: Rc<dyn Fn(&str)>,
        on_open_change: Rc<dyn Fn(bool)>,
    ) -> Rc<TimeframeQuick> {
        let doc = &env.doc;
        let input = make(doc, "input", "vela-tq-input");
        attr(&input, "type", "text");
        attr(&input, "spellcheck", "false");
        attr(&input, "autocomplete", "off");
        attr(&input, "aria-label", &env.t(&title_message()));
        let hint = make(doc, "div", "vela-tq-hint");
        attr(&hint, "role", "status");
        let dialog = Dialog::new(
            env,
            DialogOpts {
                title: env.t(&title_message()),
                flush: false,
                draggable: true,
                on_open_change: Some(on_open_change),
            },
        );
        dialog.body().append_child(&input).ok();
        dialog.body().append_child(&hint).ok();
        let this = Rc::new_cyclic(|me| TimeframeQuick {
            env: env.clone(),
            me: me.clone(),
            dialog,
            input: input.clone(),
            hint,
            on_apply,
            perm: RefCell::new(Vec::new()),
        });
        let me = this.me.clone();
        let l1 = on(&input, "input", move |_| {
            let Some(q) = me.upgrade() else { return };
            let value = vst_value(&q.input);
            let upper = value.to_uppercase();
            if upper != value {
                // Keep the caret where the user typed (writing the value moves it to the end).
                let sel = super::js::selection_of(&q.input);
                vst_set_value(&q.input, &upper);
                if let Some((start, end)) = sel {
                    super::js::vst_set_selection(&q.input, start, end);
                }
            }
            q.render_hint();
        });
        let me = this.me.clone();
        let l2 = on(&input, "keydown", move |ev| {
            if vst_key(&ev) != "Enter" {
                return;
            }
            let Some(q) = me.upgrade() else { return };
            if let Some(canonical) = timeframe_to_apply(&vst_value(&q.input)) {
                q.close();
                (q.on_apply)(&canonical);
            }
        });
        *this.perm.borrow_mut() = vec![l1, l2];
        this
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_open()
    }

    pub fn relabel(&self) {
        self.dialog.set_title(&self.env.t(&title_message()));
        attr(&self.input, "aria-label", &self.env.t(&title_message()));
    }

    pub fn open(&self, seed: &str) {
        self.relabel();
        self.dialog.show();
        vst_set_value(&self.input, &seed_entry(seed));
        self.render_hint();
        let me = self.me.clone();
        self.env.after(0.0, move || {
            if let Some(q) = me.upgrade()
                && q.is_open()
            {
                vela_focus(&q.input);
                vela_caret_end(&q.input);
            }
        });
    }

    /// Text typed at the entry that never reached its field: closed, it opens seeded with
    /// the text; open, it extends the entry.
    pub fn type_text(&self, text: &str) {
        if !self.is_open() {
            self.open(text);
            return;
        }
        vst_set_value(&self.input, &extend_entry(&vst_value(&self.input), text));
        self.render_hint();
    }

    pub fn close(&self) {
        self.dialog.hide();
    }

    fn render_hint(&self) {
        let tr = self.env.translator();
        let hint = timeframe_hint(&vst_value(&self.input), tr.as_deref());
        self.hint.set_text_content(Some(&hint.text));
        if hint.invalid {
            attr(&self.hint, "data-invalid", "1");
        } else {
            let _ = self.hint.remove_attribute("data-invalid");
        }
    }

    pub fn destroy(&self) {
        self.dialog.destroy();
        let l: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(l);
    }
}

// ── timeframe drawer ──

pub struct TimeframeDrawer {
    env: Env,
    me: Weak<TimeframeDrawer>,
    drawer: Rc<Drawer>,
    timeframes: Vec<String>,
    current_timeframe: Rc<dyn Fn() -> String>,
    active_range: Rc<dyn Fn() -> Option<String>>,
    on_timeframe: Rc<dyn Fn(&str)>,
    on_range: Rc<dyn Fn(&str)>,
    listeners: RefCell<Vec<Listener>>,
}

pub struct TimeframeDrawerOpts {
    pub timeframes: Vec<String>,
    pub current_timeframe: Rc<dyn Fn() -> String>,
    pub active_range: Rc<dyn Fn() -> Option<String>>,
    pub on_timeframe: Rc<dyn Fn(&str)>,
    /// Gets the range chip's id (`1D`, `3M`, ...).
    pub on_range: Rc<dyn Fn(&str)>,
    pub on_open_change: Rc<dyn Fn(bool)>,
}

impl TimeframeDrawer {
    pub fn new(env: &Env, o: TimeframeDrawerOpts) -> Rc<TimeframeDrawer> {
        Rc::new_cyclic(|me| TimeframeDrawer {
            env: env.clone(),
            me: me.clone(),
            drawer: Drawer::new(env, None, Some(o.on_open_change)),
            timeframes: o.timeframes,
            current_timeframe: o.current_timeframe,
            active_range: o.active_range,
            on_timeframe: o.on_timeframe,
            on_range: o.on_range,
            listeners: RefCell::new(Vec::new()),
        })
    }

    pub fn is_open(&self) -> bool {
        self.drawer.is_open()
    }

    /// Selection state is re-read on every open: it mirrors whatever last changed the
    /// timeframe (quick entry, API, a range chip).
    pub fn open(&self) {
        let tr = self.env.translator();
        let view = timeframe_drawer_view(
            &RANGE_PRESETS,
            &self.timeframes,
            &(self.current_timeframe)(),
            (self.active_range)().as_deref(),
            tr.as_deref(),
        );
        let doc = &self.env.doc;
        let body = self.drawer.body();
        let old: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        clear(body);
        let mut l = Vec::new();
        let heading = |text: &str| {
            let h = make(doc, "div", "vela-tfd-heading");
            h.set_text_content(Some(text));
            h
        };
        body.append_child(&heading(&view.range_heading)).ok();
        let ranges = make(doc, "div", "vela-tfd-ranges");
        for chip in &view.ranges {
            let c = make(doc, "button", "vela-tfd-chip");
            attr(&c, "type", "button");
            attr(&c, "data-id", &chip.id);
            c.set_text_content(Some(&chip.label));
            if chip.active {
                attr(&c, "data-active", "1");
            }
            let (me, id) = (self.me.clone(), chip.id.clone());
            l.push(on(&c, "click", move |_| {
                if let Some(d) = me.upgrade() {
                    (d.on_range)(&id);
                    d.drawer.hide();
                }
            }));
            ranges.append_child(&c).ok();
        }
        body.append_child(&ranges).ok();
        body.append_child(&heading(&view.timeframes_heading)).ok();
        let grid = make(doc, "div", "vela-tfd-grid");
        for chip in &view.timeframes {
            let c = make(doc, "button", "vela-tfd-chip");
            attr(&c, "type", "button");
            attr(&c, "data-id", &chip.id);
            c.set_text_content(Some(&chip.label));
            if chip.active {
                attr(&c, "data-active", "1");
            }
            let (me, id) = (self.me.clone(), chip.id.clone());
            l.push(on(&c, "click", move |_| {
                if let Some(d) = me.upgrade() {
                    (d.on_timeframe)(&id);
                    d.drawer.hide();
                }
            }));
            grid.append_child(&c).ok();
        }
        body.append_child(&grid).ok();
        *self.listeners.borrow_mut() = l;
        self.env.retire(Box::new(old));
        self.drawer.show();
    }

    pub fn close(&self) {
        self.drawer.hide();
    }

    pub fn destroy(&self) {
        self.drawer.destroy();
        self.listeners.borrow_mut().clear();
    }
}

// ── timezone drawer ──

pub struct TimezoneDrawer {
    env: Env,
    me: Weak<TimezoneDrawer>,
    drawer: Rc<Drawer>,
    timezone: Rc<dyn Fn() -> String>,
    on_timezone: Rc<dyn Fn(&str)>,
    listeners: RefCell<Vec<Listener>>,
}

impl TimezoneDrawer {
    pub fn new(
        env: &Env,
        timezone: Rc<dyn Fn() -> String>,
        on_timezone: Rc<dyn Fn(&str)>,
        on_open_change: Rc<dyn Fn(bool)>,
    ) -> Rc<TimezoneDrawer> {
        Rc::new_cyclic(|me| TimezoneDrawer {
            env: env.clone(),
            me: me.clone(),
            drawer: Drawer::new(env, Some(&env.t(&tz_title())), Some(on_open_change)),
            timezone,
            on_timezone,
            listeners: RefCell::new(Vec::new()),
        })
    }

    pub fn is_open(&self) -> bool {
        self.drawer.is_open()
    }

    pub fn open(&self) {
        self.drawer.set_title(&self.env.t(&tz_title()));
        let tr = self.env.translator();
        let rows = timezone_drawer_rows(
            &(self.timezone)(),
            tr.as_deref(),
            self.env.platform.now_ms(),
        );
        let doc = &self.env.doc;
        let body = self.drawer.body();
        let old: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        clear(body);
        let list = make(doc, "div", "vela-tzd-list");
        let mut l = Vec::new();
        for row in rows {
            let el = make(doc, "div", "vela-tzd-row");
            attr(&el, "role", "button");
            attr(&el, "tabindex", "0");
            attr(&el, "data-value", &row.value);
            let label = make(doc, "span", "vela-tzd-row-label");
            label.set_text_content(Some(&row.label));
            el.append_child(&label).ok();
            if row.checked {
                el.append_child(&icon_el(doc, "check")).ok();
            }
            let (me, value) = (self.me.clone(), row.value.clone());
            let (me2, value2) = (me.clone(), value.clone());
            l.push(on(&el, "click", move |_| {
                if let Some(d) = me.upgrade() {
                    (d.on_timezone)(&value);
                    d.drawer.hide();
                }
            }));
            // A focusable row must also act from the keyboard.
            l.push(on(&el, "keydown", move |ev| {
                let k = vst_key(&ev);
                if k == "Enter" || k == " " {
                    super::js::vst_prevent(&ev);
                    if let Some(d) = me2.upgrade() {
                        (d.on_timezone)(&value2);
                        d.drawer.hide();
                    }
                }
            }));
            list.append_child(&el).ok();
        }
        body.append_child(&list).ok();
        *self.listeners.borrow_mut() = l;
        self.env.retire(Box::new(old));
        self.drawer.show();
    }

    pub fn close(&self) {
        self.drawer.hide();
    }

    pub fn destroy(&self) {
        self.drawer.destroy();
        self.listeners.borrow_mut().clear();
    }
}
