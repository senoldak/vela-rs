//! The toast surface (`widget/toast.ts`): the status pill at the bottom center of the grid.
//!
//! The logic (one message at a time, the auto-hide timer on the platform clock) is
//! [`vela_core::widget::toast::ToastModel`]; this is its view: one `div.vela-toast` that
//! shows `data-open` and `data-type` from the model's state.

use std::rc::Rc;

use vela_core::runtime::Platform;
use vela_core::widget::contributions::ToastKind as WidgetKind;
use vela_core::widget::toast::{ToastModel, ToastState};
use vela_core::workspace::ToastKind;
use web_sys::{Document, Element};

use crate::dom::make;

/// The pill and its model.
pub struct ToastView {
    model: ToastModel,
    el: Element,
}

fn paint(el: &Element, state: &ToastState) {
    if state.open {
        el.set_text_content(Some(&state.message));
        let _ = el.set_attribute("data-type", state.kind.as_str());
        let _ = el.set_attribute("data-open", "1");
    } else {
        let _ = el.remove_attribute("data-open");
    }
}

impl ToastView {
    /// A pill appended to `host` (the grid).
    pub fn new(doc: &Document, host: &Element, platform: Rc<dyn Platform>) -> Rc<Self> {
        let el = make(doc, "div", "vela-toast");
        let _ = el.set_attribute("role", "status");
        let _ = host.append_child(&el);
        let painted = el.clone();
        let model = ToastModel::new(platform, move |state| paint(&painted, state));
        Rc::new(Self { model, el })
    }

    /// Show a message; auto-hides after `ms` (0: until [`hide`](Self::hide)).
    pub fn show(&self, message: &str, kind: ToastKind, ms: f64) {
        let kind = match kind {
            ToastKind::Info => WidgetKind::Info,
            ToastKind::Success => WidgetKind::Success,
            ToastKind::Error => WidgetKind::Error,
        };
        self.model.show(message, kind, ms);
    }

    pub fn hide(&self) {
        self.model.hide();
    }

    /// What the pill shows now.
    pub fn state(&self) -> ToastState {
        self.model.state()
    }

    /// The pill's element.
    pub fn element(&self) -> &Element {
        &self.el
    }

    /// Hide, cancel the timer and remove the element.
    pub fn destroy(&self) {
        self.model.destroy();
        self.el.remove();
    }
}
