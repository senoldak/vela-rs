//! The scroll-to-latest button (`makeScrollToRealtimeButton` and its neighbours in the
//! original's `NativeRenderer`). When it shows and where it sits is decided by the core and
//! arrives through `scroll_button`; the pointer proximity that reveals it on desktop is fed
//! from the plot listeners in [`ChromeUi`]. A press glides the view back to the latest bar.

use std::cell::RefCell;
use std::rc::Rc;

use vela_core::i18n::msg;
use vela_core::render::native::ScrollButtonState;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::ChromeUi;
use super::icons::icon_at;
use crate::dom::{Listener, attr, make, on, set_style, set_styles, stop_propagation};

const SIZE_PX: f64 = 26.0;
const SIZE_TOUCH_PX: f64 = 24.0;
const ICON_PX: u32 = 12;

#[derive(Default)]
pub struct ScrollButton {
    el: RefCell<Option<Element>>,
    listener: RefCell<Option<Listener>>,
}

impl ScrollButton {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChromeUi {
    fn scroll_label(&self) -> String {
        self.text(&msg(
            "chart.scrollToLatest.label",
            "Scroll to most recent bar",
        ))
    }

    fn scroll_title(&self) -> String {
        self.text(&msg(
            "chart.scrollToLatest.title",
            "Scroll to most recent bar (Alt+Shift+→)",
        ))
    }

    pub(crate) fn build_scroll_button(self: &Rc<Self>) {
        let el = make(&self.doc, "button", "vela-scroll-latest");
        attr(&el, "type", "button");
        attr(&el, "aria-label", &self.scroll_label());
        attr(&el, "title", &self.scroll_title());
        el.set_inner_html(&icon_at("chevrons-right", ICON_PX));
        set_styles(
            &el,
            &[
                ("display", "none"),
                ("width", &format!("{SIZE_PX}px")),
                ("height", &format!("{SIZE_PX}px")),
            ],
        );
        let weak = self.weak();
        let listener = on(&el, "click", move |ev: JsValue| {
            stop_propagation(&ev);
            if let Some(renderer) = weak.upgrade().and_then(|ui| ui.renderer()) {
                renderer.scroll_to_realtime();
            }
        });
        let _ = self.root.append_child(&el);
        *self.scroll.el.borrow_mut() = Some(el);
        *self.scroll.listener.borrow_mut() = Some(listener);
    }

    /// The core's decision: show / hide, geometry, touch variant.
    pub(crate) fn scroll_button_state(&self, state: &ScrollButtonState) {
        let guard = self.scroll.el.borrow();
        let Some(el) = guard.as_ref() else { return };
        let size = if state.touch { SIZE_TOUCH_PX } else { SIZE_PX };
        set_styles(
            el,
            &[
                ("display", if state.visible { "flex" } else { "none" }),
                ("bottom", &format!("{}px", state.bottom_px)),
                ("right", &format!("{}px", state.right_px)),
                ("width", &format!("{size}px")),
                ("height", &format!("{size}px")),
            ],
        );
        set_style(el, "pointer-events", "auto");
    }

    pub(crate) fn scroll_relabel(&self) {
        if let Some(el) = self.scroll.el.borrow().as_ref() {
            attr(el, "aria-label", &self.scroll_label());
            attr(el, "title", &self.scroll_title());
        }
    }

    pub(crate) fn scroll_dispose(&self) {
        let taken_1 = self.scroll.el.borrow_mut().take();
        if let Some(el) = taken_1 {
            el.remove();
        }
        self.scroll.listener.borrow_mut().take();
    }
}
