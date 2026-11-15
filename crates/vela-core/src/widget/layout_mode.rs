//! Layout mode: the shell's size class. The widget is embeddable, so the honest
//! signal is the CONTAINER width, not the viewport: a 380px widget on a desktop page
//! gets the mobile chrome, a full-screen tablet widget keeps the desktop one unless
//! the pointer is coarse and the container is narrow enough that desktop affordances
//! (hover flyouts, 30px targets) stop working.
//!
//! The decision is pure ([`resolve_layout_mode`]) and so is the transition tracking
//! ([`LayoutModeController`]). Measuring the container (`ResizeObserver`), the
//! `(pointer: coarse)` media query and writing the `data-layout` attribute are the
//! browser crate's: it feeds [`LayoutModeController::evaluate`] with each measurement.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::runtime::Unsubscribe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    Mobile,
    Desktop,
}

impl LayoutMode {
    /// The `data-layout` attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            LayoutMode::Mobile => "mobile",
            LayoutMode::Desktop => "desktop",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutModeOption {
    #[default]
    Auto,
    Mobile,
    Desktop,
}

/// Below this container width the shell is unconditionally mobile.
pub const MOBILE_BREAKPOINT_PX: f64 = 640.0;
/// With a coarse pointer (touch-first device) the mobile chrome applies wider:
/// desktop affordances assume hover, which a coarse pointer does not have.
pub const COARSE_BREAKPOINT_PX: f64 = 920.0;

/// Resolve the effective layout mode. Pure: the controller feeds it measurements.
/// An unmeasured container (width 0: display:none, not yet attached) stays desktop
/// until a real measurement arrives, so nothing flashes mobile during construction.
pub fn resolve_layout_mode(
    option: LayoutModeOption,
    width: f64,
    coarse_pointer: bool,
) -> LayoutMode {
    match option {
        LayoutModeOption::Mobile => return LayoutMode::Mobile,
        LayoutModeOption::Desktop => return LayoutMode::Desktop,
        LayoutModeOption::Auto => {}
    }
    // A NaN width compares false everywhere and reads as unmeasured.
    if width.is_nan() || width <= 0.0 {
        return LayoutMode::Desktop;
    }
    if width < MOBILE_BREAKPOINT_PX || (coarse_pointer && width < COARSE_BREAKPOINT_PX) {
        return LayoutMode::Mobile;
    }
    LayoutMode::Desktop
}

type ModeListener = Rc<dyn Fn(LayoutMode)>;

/// Keeps the resolved mode of one shell current. Change listeners fire only on actual
/// transitions, after [`current`](Self::current) already reports the new mode.
pub struct LayoutModeController {
    option: LayoutModeOption,
    mode: Cell<LayoutMode>,
    next_id: Cell<u64>,
    listeners: Rc<RefCell<Vec<(u64, ModeListener)>>>,
}

impl LayoutModeController {
    /// `width` and `coarse_pointer` are the first measurement.
    pub fn new(option: LayoutModeOption, width: f64, coarse_pointer: bool) -> Self {
        Self {
            option,
            mode: Cell::new(resolve_layout_mode(option, width, coarse_pointer)),
            next_id: Cell::new(0),
            listeners: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn current(&self) -> LayoutMode {
        self.mode.get()
    }

    /// Subscribe to mode transitions (fires with the NEW mode).
    pub fn on_change(&self, cb: impl Fn(LayoutMode) + 'static) -> Unsubscribe {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.listeners.borrow_mut().push((id, Rc::new(cb)));
        let listeners = Rc::downgrade(&self.listeners);
        Box::new(move || {
            if let Some(l) = listeners.upgrade() {
                l.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Re-resolve after a container resize or a pointer-media change. Returns the new
    /// mode when it changed.
    pub fn evaluate(&self, width: f64, coarse_pointer: bool) -> Option<LayoutMode> {
        let next = resolve_layout_mode(self.option, width, coarse_pointer);
        if next == self.mode.get() {
            return None;
        }
        self.mode.set(next);
        let snapshot: Vec<ModeListener> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, cb)| Rc::clone(cb))
            .collect();
        for cb in snapshot {
            cb(next);
        }
        Some(next)
    }

    pub fn destroy(&self) {
        self.listeners.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LayoutModeOption::{Auto, Desktop, Mobile};

    #[test]
    fn pins_the_mode_when_the_option_is_explicit() {
        assert_eq!(
            resolve_layout_mode(Mobile, 1920.0, false),
            LayoutMode::Mobile
        );
        assert_eq!(
            resolve_layout_mode(Desktop, 320.0, true),
            LayoutMode::Desktop
        );
    }

    #[test]
    fn auto_narrow_containers_are_mobile_wide_ones_desktop() {
        assert_eq!(
            resolve_layout_mode(Auto, MOBILE_BREAKPOINT_PX - 1.0, false),
            LayoutMode::Mobile
        );
        assert_eq!(
            resolve_layout_mode(Auto, MOBILE_BREAKPOINT_PX, false),
            LayoutMode::Desktop
        );
        assert_eq!(
            resolve_layout_mode(Auto, 1920.0, false),
            LayoutMode::Desktop
        );
    }

    #[test]
    fn auto_a_coarse_pointer_widens_the_mobile_band_but_not_indefinitely() {
        assert_eq!(
            resolve_layout_mode(Auto, COARSE_BREAKPOINT_PX - 1.0, true),
            LayoutMode::Mobile
        );
        assert_eq!(
            resolve_layout_mode(Auto, COARSE_BREAKPOINT_PX, true),
            LayoutMode::Desktop
        );
        assert_eq!(
            resolve_layout_mode(Auto, COARSE_BREAKPOINT_PX - 1.0, false),
            LayoutMode::Desktop
        );
    }

    #[test]
    fn auto_an_unmeasured_container_stays_desktop() {
        assert_eq!(resolve_layout_mode(Auto, 0.0, true), LayoutMode::Desktop);
        assert_eq!(resolve_layout_mode(Auto, -1.0, true), LayoutMode::Desktop);
        assert_eq!(
            resolve_layout_mode(Auto, f64::NAN, true),
            LayoutMode::Desktop
        );
    }

    #[test]
    fn the_controller_notifies_only_on_transitions() {
        let ctl = LayoutModeController::new(Auto, 0.0, false);
        assert_eq!(ctl.current(), LayoutMode::Desktop);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen2 = Rc::clone(&seen);
        let off = ctl.on_change(move |m| seen2.borrow_mut().push(m));
        assert_eq!(ctl.evaluate(1000.0, false), None);
        assert_eq!(ctl.evaluate(300.0, false), Some(LayoutMode::Mobile));
        assert_eq!(ctl.evaluate(310.0, false), None);
        assert_eq!(ctl.evaluate(900.0, false), Some(LayoutMode::Desktop));
        off();
        assert_eq!(ctl.evaluate(300.0, false), Some(LayoutMode::Mobile));
        assert_eq!(*seen.borrow(), [LayoutMode::Mobile, LayoutMode::Desktop]);
    }

    #[test]
    fn a_pinned_controller_never_transitions() {
        let ctl = LayoutModeController::new(Mobile, 2000.0, false);
        assert_eq!(ctl.current(), LayoutMode::Mobile);
        assert_eq!(ctl.evaluate(2000.0, false), None);
        assert_eq!(LayoutMode::Mobile.as_str(), "mobile");
    }
}
