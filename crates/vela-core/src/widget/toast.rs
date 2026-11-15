//! Toast: the widget's feedback pill (bottom-center, auto-hides).
//!
//! The pure half of the original `Toast`: the queue of ONE message and its timing
//! ([`ToastModel`], on the [`Platform`] clock). Left for the browser crate: the element, its
//! CSS and the `data-open` / `data-type` attributes the view derives from
//! [`ToastState`] (it repaints from the callback given to [`ToastModel::new`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::contributions::ToastKind;
use crate::runtime::{Platform, TimerId};

/// The default auto-hide delay.
pub const DEFAULT_TOAST_MS: f64 = 3000.0;

/// What the pill shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToastState {
    pub message: String,
    pub kind: ToastKind,
    pub open: bool,
}

impl ToastKind {
    /// The `data-type` attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            ToastKind::Info => "info",
            ToastKind::Success => "success",
            ToastKind::Error => "error",
        }
    }
}

struct Inner {
    platform: Rc<dyn Platform>,
    state: RefCell<ToastState>,
    timer: Cell<Option<TimerId>>,
    on_change: Rc<dyn Fn(&ToastState)>,
}

impl Inner {
    fn clear_timer(&self) {
        if let Some(id) = self.timer.take() {
            self.platform.clear_timeout(id);
        }
    }

    fn hide(&self) {
        self.clear_timer();
        self.state.borrow_mut().open = false;
        let snapshot = self.state.borrow().clone();
        (self.on_change)(&snapshot);
    }
}

/// One message at a time: a new one replaces the showing one and restarts the timer.
pub struct ToastModel {
    inner: Rc<Inner>,
}

impl ToastModel {
    pub fn new(platform: Rc<dyn Platform>, on_change: impl Fn(&ToastState) + 'static) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                state: RefCell::new(ToastState::default()),
                timer: Cell::new(None),
                on_change: Rc::new(on_change),
            }),
        }
    }

    pub fn state(&self) -> ToastState {
        self.inner.state.borrow().clone()
    }

    /// Show a message; auto-hides after `ms` (0 = sticky until [`hide`](Self::hide)).
    pub fn show(&self, message: &str, kind: ToastKind, ms: f64) {
        self.inner.clear_timer();
        {
            let mut st = self.inner.state.borrow_mut();
            st.message = message.to_string();
            st.kind = kind;
            st.open = true;
        }
        let snapshot = self.state();
        (self.inner.on_change)(&snapshot);
        if ms > 0.0 {
            let weak = Rc::downgrade(&self.inner);
            let id = self.inner.platform.set_timeout(
                ms,
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.timer.set(None);
                        inner.hide();
                    }
                }),
            );
            self.inner.timer.set(Some(id));
        }
    }

    pub fn hide(&self) {
        self.inner.hide();
    }

    pub fn destroy(&self) {
        self.inner.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;

    fn rig() -> (ManualPlatform, ToastModel, Rc<RefCell<Vec<ToastState>>>) {
        let platform = ManualPlatform::new(0.0);
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        let toast = ToastModel::new(platform.handle(), move |s| {
            sink.borrow_mut().push(s.clone())
        });
        (platform, toast, log)
    }

    #[test]
    fn a_message_shows_and_auto_hides_after_its_delay() {
        let (platform, toast, log) = rig();
        toast.show("Saved", ToastKind::Success, DEFAULT_TOAST_MS);
        assert_eq!(
            toast.state(),
            ToastState {
                message: "Saved".into(),
                kind: ToastKind::Success,
                open: true
            }
        );
        platform.advance(2999.0);
        assert!(toast.state().open);
        platform.advance(2.0);
        assert!(!toast.state().open);
        assert_eq!(log.borrow().len(), 2);
        assert_eq!(
            toast.state().message,
            "Saved",
            "the text stays for the exit animation"
        );
    }

    #[test]
    fn a_new_message_replaces_the_showing_one_and_restarts_the_timer() {
        let (platform, toast, _) = rig();
        toast.show("one", ToastKind::Info, 3000.0);
        platform.advance(2000.0);
        toast.show("two", ToastKind::Error, 3000.0);
        platform.advance(2000.0);
        assert!(
            toast.state().open,
            "the first timer must not hide the second message"
        );
        assert_eq!(
            (toast.state().message.as_str(), toast.state().kind),
            ("two", ToastKind::Error)
        );
        platform.advance(1001.0);
        assert!(!toast.state().open);
    }

    #[test]
    fn zero_is_sticky_until_hidden() {
        let (platform, toast, _) = rig();
        toast.show("Stay", ToastKind::Info, 0.0);
        platform.advance(1_000_000.0);
        assert!(toast.state().open);
        toast.hide();
        assert!(!toast.state().open);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn hiding_cancels_the_pending_timer() {
        let (platform, toast, log) = rig();
        toast.show("x", ToastKind::Info, 3000.0);
        toast.hide();
        assert_eq!(platform.pending_timers(), 0);
        platform.advance(10_000.0);
        assert_eq!(log.borrow().len(), 2, "no late hide fires");
        toast.destroy();
    }

    #[test]
    fn kinds_spell_their_data_type() {
        assert_eq!(ToastKind::Info.as_str(), "info");
        assert_eq!(ToastKind::Success.as_str(), "success");
        assert_eq!(ToastKind::Error.as_str(), "error");
    }
}
