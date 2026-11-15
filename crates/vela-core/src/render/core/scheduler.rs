//! Frame scheduling for the native renderer: repaint requests coalesce into
//! one frame at the highest requested level.
//!
//! The original arms `requestAnimationFrame` itself. Here the host passes a
//! "request a frame" callback (and the matching cancel) when it builds the
//! [`Scheduler`], so the coalescing and ordering logic stays platform-neutral.
//! The browser crate supplies `requestAnimationFrame` / `cancelAnimationFrame`.
//!
//! A [`Scheduler`] is a handle owned by one caller. The callbacks are `Fn`
//! (shared, not `FnMut`) because `flush_now` may be called from inside the
//! frame callback; a renderer keeps its mutable state behind interior
//! mutability, as it must to be reachable from the scheduled frame anyway.

use std::cell::Cell;
use std::rc::Rc;

/// Invalidation level: higher levels subsume lower ones within a frame.
///
///  - `None`:   nothing to redraw.
///  - `Cursor`: only the crosshair/overlay moved (cheapest; viewport unchanged).
///  - `Chrome`: only the chrome layer's content moved (the countdown chip's
///    wall-clock tick): repaint chrome + crosshair over the frame's existing
///    scales; the geometry/volume/VPVR/SDK layers stay untouched.
///  - `Light`:  data unchanged but a value/style changed; repaint data + chrome.
///  - `Full`:   viewport/layout/data changed; repaint everything.
///
/// Invariant enforced by callers: any viewport change must invalidate at
/// `Light` or `Full` (never `Cursor`/`Chrome`) so data and chrome repaint in
/// the SAME frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InvalidateLevel {
    #[default]
    None = 0,
    Cursor = 1,
    Chrome = 2,
    Light = 3,
    Full = 4,
}

/// Whether a frame at this level must repaint the DATA layer (series, fills,
/// drawings, axes, grid). `Cursor` repaints only the crosshair overlay and
/// `Chrome` only the chrome canvas; `Light`/`Full` repaint the data layer too.
/// The crosshair overlay is repainted on every frame regardless (it is cheap
/// and must stay aligned after a pan/zoom).
pub fn repaints_data(level: InvalidateLevel) -> bool {
    level >= InvalidateLevel::Light
}

/// Whether a frame at this level must repaint the CHROME layer (axes, Pine
/// drawings, price line + countdown chips). Data-tier frames repaint chrome
/// too.
pub fn repaints_chrome(level: InvalidateLevel) -> bool {
    level >= InvalidateLevel::Chrome
}

/// What the host runs on its next animation frame.
pub type FrameTask = Box<dyn FnOnce()>;
/// Arm one animation frame; the returned handle is what [`CancelFrame`] takes.
pub type RequestFrame = Rc<dyn Fn(FrameTask) -> u32>;
pub type CancelFrame = Rc<dyn Fn(u32)>;
pub type OnFrame = Rc<dyn Fn(InvalidateLevel)>;

struct Inner {
    level: Cell<InvalidateLevel>,
    handle: Cell<Option<u32>>,
    on_frame: OnFrame,
    request: RequestFrame,
    cancel: CancelFrame,
}

impl Inner {
    fn flush(&self) {
        self.handle.set(None);
        let level = self.level.replace(InvalidateLevel::None);
        if level != InvalidateLevel::None {
            (self.on_frame)(level);
        }
    }
}

/// A re-armable single-frame scheduler: many `invalidate()` calls within a
/// frame coalesce into one repaint at the highest requested level. Static
/// charts cost zero idle frames (no frame is armed when nothing is invalid).
pub struct Scheduler {
    inner: Rc<Inner>,
}

impl Scheduler {
    pub fn new(on_frame: OnFrame, request: RequestFrame, cancel: CancelFrame) -> Self {
        Self {
            inner: Rc::new(Inner {
                level: Cell::new(InvalidateLevel::None),
                handle: Cell::new(None),
                on_frame,
                request,
                cancel,
            }),
        }
    }

    pub fn invalidate(&self, level: InvalidateLevel) {
        if level <= InvalidateLevel::None {
            return;
        }
        let inner = &self.inner;
        if level > inner.level.get() {
            inner.level.set(level);
        }
        if inner.handle.get().is_none() {
            // The pending task holds the scheduler weakly: dropping it
            // leaves a frame that does nothing.
            let weak = Rc::downgrade(inner);
            let handle = (inner.request)(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.flush();
                }
            }));
            inner.handle.set(Some(handle));
        }
    }

    /// Force a synchronous repaint now (used by resize so layout is immediate).
    pub fn flush_now(&self, level: InvalidateLevel) {
        let inner = &self.inner;
        if let Some(handle) = inner.handle.take() {
            (inner.cancel)(handle);
        }
        if level > inner.level.get() {
            inner.level.set(level);
        }
        inner.flush();
    }

    /// Whether a frame is armed.
    pub fn is_armed(&self) -> bool {
        self.inner.handle.get().is_some()
    }

    pub fn destroy(&self) {
        let inner = &self.inner;
        if let Some(handle) = inner.handle.take() {
            (inner.cancel)(handle);
        }
        inner.level.set(InvalidateLevel::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Controllable frame source: capture the pending task so the test can fire frames.
    struct Harness {
        scheduler: Scheduler,
        frames: Rc<RefCell<Vec<InvalidateLevel>>>,
        pending: Rc<RefCell<Option<FrameTask>>>,
    }

    impl Harness {
        fn new() -> Self {
            let pending: Rc<RefCell<Option<FrameTask>>> = Rc::new(RefCell::new(None));
            let frames = Rc::new(RefCell::new(Vec::new()));
            let (p1, p2, f) = (pending.clone(), pending.clone(), frames.clone());
            let scheduler = Scheduler::new(
                Rc::new(move |lvl| f.borrow_mut().push(lvl)),
                Rc::new(move |task| {
                    *p1.borrow_mut() = Some(task);
                    1
                }),
                Rc::new(move |_| {
                    *p2.borrow_mut() = None;
                }),
            );
            Self {
                scheduler,
                frames,
                pending,
            }
        }

        fn fire(&self) {
            let task = self.pending.borrow_mut().take();
            if let Some(task) = task {
                task();
            }
        }

        fn is_armed(&self) -> bool {
            self.pending.borrow().is_some()
        }

        fn frames(&self) -> Vec<InvalidateLevel> {
            self.frames.borrow().clone()
        }
    }

    use InvalidateLevel::{Chrome, Cursor, Full, Light, None as Nothing};

    #[test]
    fn coalesces_multiple_invalidations_in_a_frame_to_the_highest_level() {
        let h = Harness::new();
        h.scheduler.invalidate(Cursor);
        h.scheduler.invalidate(Full);
        h.scheduler.invalidate(Light);
        assert!(h.is_armed());
        h.fire();
        assert_eq!(h.frames(), vec![Full]);
    }

    #[test]
    fn re_arms_for_the_next_frame_after_flushing() {
        let h = Harness::new();
        h.scheduler.invalidate(Light);
        h.fire();
        h.scheduler.invalidate(Cursor);
        assert!(h.is_armed());
        h.fire();
        assert_eq!(h.frames(), vec![Light, Cursor]);
    }

    #[test]
    fn none_is_a_no_op() {
        let h = Harness::new();
        h.scheduler.invalidate(Nothing);
        assert!(!h.is_armed());
        h.fire();
        assert!(h.frames().is_empty());
    }

    #[test]
    fn flush_now_paints_synchronously() {
        let h = Harness::new();
        h.scheduler.flush_now(Full);
        assert_eq!(h.frames(), vec![Full]);
        assert!(!h.is_armed());
    }

    #[test]
    fn flush_now_cancels_an_armed_frame_and_keeps_the_highest_level() {
        let h = Harness::new();
        h.scheduler.invalidate(Light);
        h.scheduler.flush_now(Cursor);
        assert!(!h.is_armed());
        assert_eq!(h.frames(), vec![Light]);
        assert!(!h.scheduler.is_armed());
    }

    #[test]
    fn flush_now_with_nothing_pending_paints_nothing() {
        let h = Harness::new();
        h.scheduler.flush_now(Nothing);
        assert!(h.frames().is_empty());
    }

    #[test]
    fn destroy_cancels_a_pending_frame() {
        let h = Harness::new();
        h.scheduler.invalidate(Full);
        h.scheduler.destroy();
        h.fire();
        assert!(h.frames().is_empty());
    }

    #[test]
    fn a_frame_fired_after_the_scheduler_is_dropped_does_nothing() {
        let h = Harness::new();
        h.scheduler.invalidate(Full);
        let task = h.pending.borrow_mut().take().unwrap();
        drop(h.scheduler);
        task();
        assert!(h.frames.borrow().is_empty());
    }

    #[test]
    fn invalidating_inside_a_frame_arms_the_next_one() {
        let pending: Rc<RefCell<Option<FrameTask>>> = Rc::new(RefCell::new(None));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let slot: Rc<RefCell<Option<Rc<Scheduler>>>> = Rc::new(RefCell::new(None));
        let (p, s, sl) = (pending.clone(), seen.clone(), slot.clone());
        let scheduler = Rc::new(Scheduler::new(
            Rc::new(move |lvl| {
                s.borrow_mut().push(lvl);
                if lvl == Full
                    && let Some(sched) = sl.borrow().as_ref()
                {
                    sched.invalidate(Cursor);
                }
            }),
            Rc::new(move |task| {
                *p.borrow_mut() = Some(task);
                1
            }),
            Rc::new(|_| {}),
        ));
        *slot.borrow_mut() = Some(scheduler.clone());
        scheduler.invalidate(Full);
        let task = pending.borrow_mut().take().unwrap();
        task();
        assert!(pending.borrow().is_some());
        let task = pending.borrow_mut().take().unwrap();
        task();
        assert_eq!(*seen.borrow(), vec![Full, Cursor]);
        // break the Rc cycle the test built
        *slot.borrow_mut() = None;
    }

    #[test]
    fn data_layer_repaints_only_at_light_or_full() {
        assert!(!repaints_data(Nothing));
        assert!(!repaints_data(Cursor));
        assert!(!repaints_data(Chrome));
        assert!(repaints_data(Light));
        assert!(repaints_data(Full));
    }

    #[test]
    fn chrome_sits_between_cursor_and_light() {
        assert!(Cursor < Chrome);
        assert!(Chrome < Light);
    }

    #[test]
    fn chrome_repaints_from_chrome_up() {
        assert!(!repaints_chrome(Nothing));
        assert!(!repaints_chrome(Cursor));
        assert!(repaints_chrome(Chrome));
        assert!(repaints_chrome(Light));
        assert!(repaints_chrome(Full));
    }

    #[test]
    fn chrome_coalesces_with_its_neighbors_at_the_highest_level() {
        let h = Harness::new();
        h.scheduler.invalidate(Chrome);
        h.scheduler.invalidate(Cursor);
        h.fire();
        h.scheduler.invalidate(Chrome);
        h.scheduler.invalidate(Light);
        h.fire();
        assert_eq!(h.frames(), vec![Chrome, Light]);
    }

    #[test]
    fn a_lone_countdown_tick_flushes_as_one_chrome_frame() {
        let h = Harness::new();
        h.scheduler.invalidate(Chrome);
        assert!(h.is_armed());
        h.fire();
        assert_eq!(h.frames(), vec![Chrome]);
    }
}
