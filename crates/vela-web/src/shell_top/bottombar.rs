//! The bottom bar (`bottombar.ts`): the date-range chips on the left, the live clock inside
//! the time-zone trigger, the RTH / ETH toggle and the chart-settings gear on the right.
//!
//! The data (the chips, the label of a chip, the session toggle state, the zone rows) is
//! [`vela_core::widget::bottombar`]; the clock ticks on the browser's second clock.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::format::types::{ClockFormatOptions, FormatSurface};
use vela_core::i18n::english_translator;
use vela_core::ports::chart_renderer::WallClock;
use vela_core::ports::market_data::MarketSession;
use vela_core::runtime::Unsubscribe;
use vela_core::timezones::tz_button_label;
use vela_core::ui::MenuItemDescriptor;
use vela_core::widget::bottombar::{
    RANGE_PRESETS, RangePreset, SessionKey, chart_settings_label, display_zone, extended_label,
    range_label, regular_label, session_click, session_label, session_toggle_state,
    time_zone_label, timezone_menu_items,
};
use web_sys::Element;

use super::kit::{Env, MenuButton, MenuOpts, Tooltip, icon_el, set_class};
use crate::dom::{Listener, attr, make, on, set_style};
use crate::host::SecondClock;

pub struct BottombarOpts {
    pub timezone: String,
    pub exchange_timezone: Option<String>,
    pub on_range: Rc<dyn Fn(&RangePreset)>,
    pub on_timezone: Rc<dyn Fn(&str)>,
    pub on_session: Rc<dyn Fn(MarketSession)>,
    pub on_settings: Rc<dyn Fn()>,
}

pub struct Bottombar {
    env: Env,
    me: Weak<Bottombar>,
    wrap: Element,
    clock_el: Element,
    tz_label: Element,
    tz_button: Element,
    session_el: Element,
    settings_btn: Element,
    range_buttons: Vec<(String, Element)>,
    session_buttons: Vec<(SessionKey, Element)>,
    tz_menu: RefCell<Option<Rc<MenuButton>>>,
    settings_tip: Rc<Tooltip>,
    timezone: RefCell<String>,
    exchange: RefCell<Option<String>>,
    session_enabled: RefCell<bool>,
    on_range: Rc<dyn Fn(&RangePreset)>,
    on_timezone: Rc<dyn Fn(&str)>,
    on_session: Rc<dyn Fn(MarketSession)>,
    listeners: RefCell<Vec<Listener>>,
    unsub_clock: RefCell<Option<Unsubscribe>>,
}

fn session_of(key: SessionKey) -> MarketSession {
    match key {
        SessionKey::Regular => MarketSession::Regular,
        SessionKey::Extended => MarketSession::Extended,
    }
}

fn key_of(session: MarketSession) -> SessionKey {
    match session {
        MarketSession::Regular => SessionKey::Regular,
        MarketSession::Extended => SessionKey::Extended,
    }
}

impl Bottombar {
    pub fn new(env: &Env, opts: BottombarOpts) -> Rc<Bottombar> {
        let doc = &env.doc;
        // Mounted into `Region::Bottombar`, whose element is the original's
        // `.vela-widget-bottombar` flex row.
        let wrap = make(doc, "div", "vela-bottombar-content");
        set_style(&wrap, "display", "contents");
        let tr = env.translator();
        let mut range_buttons = Vec::new();
        for preset in RANGE_PRESETS {
            let b = make(doc, "button", "vela-bb-range");
            attr(&b, "type", "button");
            attr(&b, "data-range", preset.id);
            b.set_text_content(Some(&range_label(preset.id, tr.as_deref())));
            wrap.append_child(&b).ok();
            range_buttons.push((preset.id.to_string(), b));
        }
        let spacer = make(doc, "span", "vela-bb-spacer");
        // The clock lives INSIDE the zone trigger: clicking the time opens the same dropdown
        // as clicking the zone label.
        let tz_button = make(doc, "button", "vela-bb-tz");
        attr(&tz_button, "type", "button");
        attr(&tz_button, "aria-label", &env.t(&time_zone_label()));
        let clock_el = make(doc, "span", "vela-bb-clock");
        let tz_label = make(doc, "span", "");
        tz_button.append_child(&clock_el).ok();
        tz_button.append_child(&tz_label).ok();
        let session_el = make(doc, "span", "vela-bb-session");
        attr(&session_el, "role", "group");
        // Boots HIDDEN: sessions only apply to markets that have them.
        set_style(&session_el, "display", "none");
        attr(&session_el, "title", &env.t(&session_label()));
        attr(&session_el, "aria-label", &env.t(&session_label()));
        let mut session_buttons = Vec::new();
        for (key, label) in [
            (SessionKey::Regular, regular_label()),
            (SessionKey::Extended, extended_label()),
        ] {
            let b = make(
                doc,
                "button",
                &format!(
                    "vela-bb-session-btn{}",
                    if key == SessionKey::Regular {
                        " is-active"
                    } else {
                        ""
                    }
                ),
            );
            attr(&b, "type", "button");
            b.set_text_content(Some(&env.t(&label)));
            super::js::vst_set_flag(&b, "disabled", true);
            attr(&b, "disabled", "");
            session_el.append_child(&b).ok();
            session_buttons.push((key, b));
        }
        let settings_btn = make(doc, "button", "vela-bb-settings");
        attr(&settings_btn, "type", "button");
        settings_btn.append_child(&icon_el(doc, "gear")).ok();
        attr(&settings_btn, "aria-label", &env.t(&chart_settings_label()));
        let settings_tip = Tooltip::new(env, &settings_btn, &env.t(&chart_settings_label()));
        for el in [&spacer, &tz_button, &session_el, &settings_btn] {
            wrap.append_child(el).ok();
        }

        let this = Rc::new_cyclic(|me| Bottombar {
            env: env.clone(),
            me: me.clone(),
            wrap,
            clock_el,
            tz_label,
            tz_button: tz_button.clone(),
            session_el,
            settings_btn: settings_btn.clone(),
            range_buttons,
            session_buttons,
            tz_menu: RefCell::new(None),
            settings_tip,
            timezone: RefCell::new(opts.timezone),
            exchange: RefCell::new(opts.exchange_timezone),
            session_enabled: RefCell::new(false),
            on_range: opts.on_range,
            on_timezone: opts.on_timezone,
            on_session: opts.on_session,
            listeners: RefCell::new(Vec::new()),
            unsub_clock: RefCell::new(None),
        });
        let mut l = Vec::new();
        for (id, b) in &this.range_buttons {
            let (me, id) = (this.me.clone(), id.clone());
            l.push(on(b, "click", move |_| {
                let Some(bar) = me.upgrade() else { return };
                bar.set_active_range(Some(&id));
                if let Some(preset) = RANGE_PRESETS.iter().find(|p| p.id == id) {
                    (bar.on_range)(preset);
                }
            }));
        }
        for (key, b) in &this.session_buttons {
            let (me, key) = (this.me.clone(), *key);
            l.push(on(b, "click", move |_| {
                let Some(bar) = me.upgrade() else { return };
                let enabled = *bar.session_enabled.borrow();
                if let Some(k) = session_click(key, enabled) {
                    // Optimistic: the reload confirms.
                    bar.set_session(session_of(k), true);
                    (bar.on_session)(session_of(k));
                }
            }));
        }
        let f = opts.on_settings;
        l.push(on(&settings_btn, "click", move |_| f()));
        *this.listeners.borrow_mut() = l;

        let me = this.me.clone();
        let me2 = this.me.clone();
        let tz = MenuButton::new(
            env,
            &tz_button,
            move || me.upgrade().map(|b| b.tz_items()).unwrap_or_default(),
            move || {
                let a = me2.clone();
                MenuOpts {
                    min_width: None,
                    icon_badges: false,
                    side_top: true,
                    align_end: true,
                    on_select: Rc::new(move |zone| {
                        if let Some(b) = a.upgrade() {
                            let exchange = b.exchange.borrow().clone();
                            b.set_timezone(zone, exchange.as_deref());
                            (b.on_timezone)(zone);
                        }
                    }),
                    on_favorite: None,
                    on_close: None,
                }
            },
        );
        *this.tz_menu.borrow_mut() = Some(tz);
        this.paint_zone();
        this.tick();
        if let Some(root) = env.root() {
            let clock = SecondClock::new(env.platform.clone());
            let me = this.me.clone();
            let unsub = clock.on_tick(Box::new(move |_| {
                if let Some(b) = me.upgrade() {
                    b.tick();
                }
            }));
            // The clock must outlive the subscription it serves: it is dropped with it.
            let keep = clock;
            *this.unsub_clock.borrow_mut() = Some(Box::new(move || {
                unsub();
                drop(keep);
            }));
            let _ = root;
        }
        this
    }

    pub fn element(&self) -> &Element {
        &self.wrap
    }

    fn display_zone(&self) -> String {
        display_zone(&self.timezone.borrow(), self.exchange.borrow().as_deref())
    }

    fn tz_items(&self) -> Vec<MenuItemDescriptor> {
        let tr = self.env.translator();
        timezone_menu_items(
            &self.timezone.borrow(),
            tr.as_deref(),
            self.env.platform.now_ms(),
        )
    }

    fn paint_zone(&self) {
        let zone = self.display_zone();
        let label = match self.env.root() {
            Some(root) => {
                tz_button_label(root.env().intl.as_ref(), &zone, self.env.platform.now_ms())
            }
            None => zone,
        };
        self.tz_label.set_text_content(Some(&label));
    }

    /// Reflect the stored choice AND the active chart's market zone. Idempotent.
    pub fn set_timezone(&self, zone: &str, exchange: Option<&str>) {
        let choice_changed = *self.timezone.borrow() != zone;
        if !choice_changed && self.exchange.borrow().as_deref() == exchange {
            return;
        }
        *self.timezone.borrow_mut() = zone.to_string();
        *self.exchange.borrow_mut() = exchange.map(str::to_string);
        self.paint_zone();
        if choice_changed && let Some(m) = self.tz_menu.borrow().as_ref() {
            m.refresh();
        }
        self.tick();
    }

    /// Highlight (or clear with `None`) the active range chip.
    pub fn set_active_range(&self, id: Option<&str>) {
        for (key, b) in &self.range_buttons {
            if id == Some(key.as_str()) {
                attr(b, "data-active", "1");
                attr(b, "aria-pressed", "true");
            } else {
                let _ = b.remove_attribute("data-active");
                attr(b, "aria-pressed", "false");
            }
        }
    }

    /// Reflect the ACTIVE chart's session posture. `enabled: false` hides the toggle.
    pub fn set_session(&self, session: MarketSession, enabled: bool) {
        *self.session_enabled.borrow_mut() = enabled;
        let state = session_toggle_state(key_of(session), enabled);
        set_style(
            &self.session_el,
            "display",
            if state.visible { "" } else { "none" },
        );
        for (key, b) in &self.session_buttons {
            super::js::vst_set_flag(b, "disabled", !state.enabled);
            if state.enabled {
                let _ = b.remove_attribute("disabled");
            } else {
                attr(b, "disabled", "");
            }
            set_class(b, "is-active", *key == state.active);
            attr(
                b,
                "aria-pressed",
                if *key == state.active {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }

    /// Re-read every label, the zone menu and the clock in the current language.
    pub fn relocalize(&self) {
        let tr = self.env.translator();
        for (id, b) in &self.range_buttons {
            b.set_text_content(Some(&range_label(id, tr.as_deref())));
        }
        attr(
            &self.tz_button,
            "aria-label",
            &self.env.t(&time_zone_label()),
        );
        attr(&self.session_el, "title", &self.env.t(&session_label()));
        attr(
            &self.session_el,
            "aria-label",
            &self.env.t(&session_label()),
        );
        for (key, b) in &self.session_buttons {
            let m = match key {
                SessionKey::Regular => regular_label(),
                SessionKey::Extended => extended_label(),
            };
            b.set_text_content(Some(&self.env.t(&m)));
        }
        attr(
            &self.settings_btn,
            "aria-label",
            &self.env.t(&chart_settings_label()),
        );
        self.settings_tip
            .set_content(&self.env.t(&chart_settings_label()));
        if let Some(m) = self.tz_menu.borrow().as_ref() {
            m.refresh();
        }
        self.paint_zone();
        self.tick();
    }

    fn tick(&self) {
        let tr = self.env.translator();
        let english = english_translator();
        let translator: &dyn vela_core::i18n::Translator = match tr.as_deref() {
            Some(t) => t,
            None => english,
        };
        let text = translator.format().clock(
            self.env.platform.now_ms(),
            &ClockFormatOptions {
                surface: Some(FormatSurface::Clock),
                time_zone: Some(self.display_zone()),
                seconds: None,
            },
        );
        self.clock_el.set_text_content(Some(&text));
    }

    pub fn destroy(&self) {
        if let Some(u) = self.unsub_clock.borrow_mut().take() {
            u();
        }
        if let Some(m) = self.tz_menu.borrow_mut().take() {
            m.destroy();
        }
        self.settings_tip.destroy();
        self.listeners.borrow_mut().clear();
        self.wrap.remove();
    }
}
