//! The symbol watermark (`watermark.ts`): a large faded `SYMBOL · TF` centered on the price
//! pane, with a smaller `Replay` line under it while the chart replays past bars.
//!
//! The visibility rules, the mark text and the font fit are
//! [`vela_core::widget::watermark`]; this is the element and the measuring.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::widget::watermark::{
    MAX_FONT_PX, WatermarkModel, mark_text, replay_message, watermark_font_px,
};
use web_sys::Element;

use super::js::{vst_client_width, vst_text_width};
use super::kit::{Env, Observer, icon_el};
use crate::dom::{attr, make, set_style};

pub struct Watermark {
    env: Env,
    me: Weak<Watermark>,
    host: Element,
    pub el: Element,
    text: Element,
    replay_line: Element,
    replay_text: Element,
    model: RefCell<WatermarkModel>,
    symbol: RefCell<String>,
    timeframe: RefCell<String>,
    last_font: Cell<f64>,
    observer: RefCell<Option<Observer>>,
}

impl Watermark {
    pub fn new(env: &Env, host: &Element, symbol: &str, timeframe: &str) -> Rc<Watermark> {
        let doc = &env.doc;
        let el = make(doc, "div", "vela-watermark");
        // Opts into the renderer's PNG export, beneath the canvases: on screen the mark sits
        // behind the candles.
        attr(&el, "data-vela-screenshot", "under");
        attr(&el, "aria-hidden", "true");
        let text = make(doc, "span", "vela-watermark-text");
        let replay_line = make(doc, "span", "vela-watermark-replay");
        let replay_text = make(doc, "span", "");
        replay_text.set_text_content(Some(&env.t(&replay_message())));
        replay_line.append_child(&icon_el(doc, "replay")).ok();
        replay_line.append_child(&replay_text).ok();
        el.append_child(&text).ok();
        el.append_child(&replay_line).ok();
        host.append_child(&el).ok();
        let this = Rc::new_cyclic(|me| Watermark {
            env: env.clone(),
            me: me.clone(),
            host: host.clone(),
            el: el.clone(),
            text,
            replay_line,
            replay_text,
            model: RefCell::new(WatermarkModel::default()),
            symbol: RefCell::new(String::new()),
            timeframe: RefCell::new(String::new()),
            last_font: Cell::new(MAX_FONT_PX),
            observer: RefCell::new(None),
        });
        // The element tracks the price pane (CSS insets on the host): observing it refits on
        // splitter drags and pane-layout changes.
        let me = this.me.clone();
        *this.observer.borrow_mut() = Some(Observer::new(&el, move || {
            if let Some(w) = me.upgrade() {
                w.fit();
            }
        }));
        this.update(symbol, timeframe);
        this.sync();
        this
    }

    pub fn set_visible(&self, visible: bool) {
        self.model.borrow_mut().set_visible(visible);
        self.sync();
    }

    pub fn set_replay_visible(&self, visible: bool) {
        self.model.borrow_mut().set_replay_visible(visible);
        self.sync();
    }

    pub fn set_replaying(&self, replaying: bool) {
        self.model.borrow_mut().set_replaying(replaying);
        self.sync();
    }

    /// Loading and the watermark never share the canvas: hidden while a load is up.
    pub fn set_loading(&self, loading: bool) {
        self.model.borrow_mut().set_loading(loading);
        self.sync();
    }

    fn sync(&self) {
        let v = self.model.borrow().visibility();
        set_style(&self.text, "display", if v.text { "" } else { "none" });
        set_style(
            &self.replay_line,
            "display",
            if v.replay_line { "" } else { "none" },
        );
        set_style(&self.el, "display", if v.shown { "" } else { "none" });
    }

    pub fn update(&self, symbol: &str, timeframe: &str) {
        *self.symbol.borrow_mut() = symbol.to_string();
        *self.timeframe.borrow_mut() = timeframe.to_string();
        let tr = self.env.translator();
        self.text
            .set_text_content(Some(&mark_text(symbol, timeframe, tr.as_deref())));
        self.fit();
    }

    /// Re-read the mark and the replay line in the shell's language.
    pub fn relocalize(&self) {
        self.replay_text
            .set_text_content(Some(&self.env.t(&replay_message())));
        let (s, t) = (
            self.symbol.borrow().clone(),
            self.timeframe.borrow().clone(),
        );
        self.update(&s, &t);
    }

    /// The text currently shown (the mark, not the replay line).
    pub fn mark(&self) -> String {
        self.text.text_content().unwrap_or_default()
    }

    /// Measure the text at the cap and shrink it to the price pane's width.
    fn fit(&self) {
        let width = f64::from(vst_client_width(&self.el) as i32);
        if width <= 0.0 || self.text.text_content().unwrap_or_default().is_empty() {
            return;
        }
        set_style(&self.el, "font-size", &format!("{MAX_FONT_PX}px"));
        let px = watermark_font_px(width, vst_text_width(&self.text));
        self.last_font.set(px);
        if px != MAX_FONT_PX {
            set_style(&self.el, "font-size", &format!("{px}px"));
        }
    }

    pub fn font_px(&self) -> f64 {
        self.last_font.get()
    }

    pub fn destroy(&self) {
        self.observer.borrow_mut().take();
        let _ = &self.host;
        self.el.remove();
    }
}
