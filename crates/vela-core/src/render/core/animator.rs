//! Time-based animation for the native renderer: the self-stopping frame loop
//! behind eased zoom, inertial pan and gliding autoscale, plus the ease
//! time-constant settings and the exponential ease itself.
//!
//! The original arms `requestAnimationFrame` and reads `performance.now()`.
//! Here the host passes a "request a frame" callback, its cancel and a `now`
//! clock (ms) when it builds the [`Animator`]; the browser crate supplies the
//! real ones.
//!
//! An [`Animator`] is a handle owned by one caller. `tick` is an `Fn` (shared,
//! not `FnMut`) because it may call [`Animator::stop`] re-entrantly, which the
//! loop honors; a renderer keeps its mutable state behind interior
//! mutability, as it must to be reachable from the scheduled frame anyway.

use std::cell::Cell;
use std::rc::Rc;

use crate::indicators::classics::math::js_min;

use super::scheduler::{CancelFrame, FrameTask, RequestFrame};

pub type Now = Rc<dyn Fn() -> f64>;
/// Called once per frame with the elapsed time (ms); returns `false` once
/// everything has settled.
pub type TickFn = Rc<dyn Fn(f64) -> bool>;

/// The longest frame gap a tick sees: a backgrounded tab (huge gap) must not
/// teleport the animation.
pub const MAX_FRAME_DT_MS: f64 = 64.0;

struct Inner {
    handle: Cell<Option<u32>>,
    last: Cell<f64>,
    stopped: Cell<bool>,
    tick: TickFn,
    request: RequestFrame,
    cancel: CancelFrame,
    now: Now,
}

impl Inner {
    fn frame_task(self: &Rc<Self>) -> FrameTask {
        let weak = Rc::downgrade(self);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.run_loop();
            }
        })
    }

    fn run_loop(self: &Rc<Self>) {
        let t = (self.now)();
        let dt = t - self.last.get();
        self.last.set(t);
        let more = (self.tick)(js_min(dt, MAX_FRAME_DT_MS));
        // Re-arm only if still wanted AND a re-entrant stop() during the tick didn't cancel us.
        let next = if more && !self.stopped.get() {
            Some((self.request)(self.frame_task()))
        } else {
            None
        };
        self.handle.set(next);
    }
}

/// A self-stopping frame loop for time-based animation (eased zoom, inertial
/// pan, gliding autoscale). Separate from the invalidation-driven
/// [`Scheduler`](super::Scheduler): while a gesture is in flight this drives a
/// continuous frame loop, calling `tick(dt_ms)` each frame; when `tick`
/// returns `false` (everything settled) it stops, so a static chart costs
/// zero idle frames again.
pub struct Animator {
    inner: Rc<Inner>,
}

impl Animator {
    pub fn new(tick: TickFn, request: RequestFrame, cancel: CancelFrame, now: Now) -> Self {
        Self {
            inner: Rc::new(Inner {
                handle: Cell::new(None),
                last: Cell::new(0.0),
                stopped: Cell::new(false),
                tick,
                request,
                cancel,
                now,
            }),
        }
    }

    pub fn active(&self) -> bool {
        self.inner.handle.get().is_some()
    }

    /// Begin (or keep) the loop. Idempotent: safe to call every gesture event.
    pub fn start(&self) {
        let inner = &self.inner;
        inner.stopped.set(false);
        if inner.handle.get().is_some() {
            return;
        }
        inner.last.set((inner.now)());
        let handle = (inner.request)(inner.frame_task());
        inner.handle.set(Some(handle));
    }

    pub fn stop(&self) {
        let inner = &self.inner;
        // Honored even if called re-entrantly from inside tick().
        inner.stopped.set(true);
        if let Some(handle) = inner.handle.take() {
            (inner.cancel)(handle);
        }
    }
}

/// One configurable ease time-constant (ms; 0 = the motion is off). Remembers
/// the last non-zero value so a plain on/off switch (the settings dialog, the
/// rich config's booleans) turns the motion back on at the host's duration,
/// not a built-in default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EaseSetting {
    ms: f64,
    on_ms: f64,
}

impl EaseSetting {
    /// `default_ms` is what the on/off switch restores when nothing else was
    /// configured; the starting value is `default_ms` too.
    pub fn new(default_ms: f64) -> Self {
        Self::with_initial(default_ms, default_ms)
    }

    /// As [`new`](Self::new) with an explicit starting value: 0 for a motion
    /// that ships off.
    pub fn with_initial(default_ms: f64, initial_ms: f64) -> Self {
        Self {
            ms: initial_ms,
            on_ms: default_ms,
        }
    }

    /// The active time-constant; 0 when off.
    pub fn tau(&self) -> f64 {
        self.ms
    }

    pub fn on(&self) -> bool {
        self.ms > 0.0
    }

    /// Set the time-constant (0 = off). A non-zero value becomes what
    /// `toggle(true)` restores.
    pub fn set(&mut self, ms: f64) {
        self.ms = ms;
        if ms > 0.0 {
            self.on_ms = ms;
        }
    }

    /// On/off only: the duration stays the last one configured.
    pub fn toggle(&mut self, on: bool) {
        self.ms = if on { self.on_ms } else { 0.0 };
    }
}

/// Frame-rate-independent exponential approach of `current` toward `target`.
pub fn ease_toward(current: f64, target: f64, dt_ms: f64, tau_ms: f64) -> f64 {
    if tau_ms <= 0.0 {
        return target;
    }
    current + (target - current) * (1.0 - (-dt_ms / tau_ms).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Controllable frame source + clock so tests step animation frames deterministically.
    struct Harness {
        animator: Animator,
        t: Rc<Cell<f64>>,
        pending: Rc<RefCell<Option<FrameTask>>>,
    }

    impl Harness {
        fn new(tick: impl Fn(f64) -> bool + 'static) -> Self {
            Self::build(|_| Rc::new(tick))
        }

        fn build(make_tick: impl FnOnce(&Rc<Cell<f64>>) -> TickFn) -> Self {
            let pending: Rc<RefCell<Option<FrameTask>>> = Rc::new(RefCell::new(None));
            let t = Rc::new(Cell::new(1000.0));
            let handles = Rc::new(Cell::new(0u32));
            let (p1, p2, t2) = (pending.clone(), pending.clone(), t.clone());
            let animator = Animator::new(
                make_tick(&t),
                Rc::new(move |task| {
                    *p1.borrow_mut() = Some(task);
                    handles.set(handles.get() + 1);
                    handles.get()
                }),
                Rc::new(move |_| {
                    *p2.borrow_mut() = None;
                }),
                Rc::new(move || t2.get()),
            );
            Self {
                animator,
                t,
                pending,
            }
        }

        fn advance(&self, dt: f64) {
            self.t.set(self.t.get() + dt);
            let task = self.pending.borrow_mut().take();
            if let Some(task) = task {
                task();
            }
        }

        fn armed(&self) -> bool {
            self.pending.borrow().is_some()
        }
    }

    #[test]
    fn runs_frames_with_dt_until_tick_returns_false_then_stops() {
        let dts = Rc::new(RefCell::new(Vec::new()));
        let frames = Rc::new(Cell::new(0));
        let (d, f) = (dts.clone(), frames.clone());
        let h = Harness::new(move |dt| {
            d.borrow_mut().push(dt);
            f.set(f.get() + 1);
            f.get() < 3 // animate for 3 frames
        });
        h.animator.start();
        assert!(h.animator.active());
        h.advance(16.0);
        h.advance(16.0);
        assert!(h.animator.active());
        h.advance(16.0);
        assert_eq!(*dts.borrow(), vec![16.0, 16.0, 16.0]);
        assert!(!h.animator.active()); // settled, loop stopped
        assert!(!h.armed());
    }

    #[test]
    fn clamps_a_huge_frame_gap_to_64ms() {
        let seen = Rc::new(Cell::new(-1.0));
        let s = seen.clone();
        let h = Harness::new(move |dt| {
            s.set(dt);
            false
        });
        h.animator.start();
        h.advance(5000.0); // tab was backgrounded for 5s
        assert_eq!(seen.get(), 64.0);
    }

    #[test]
    fn start_is_idempotent() {
        let h = Harness::new(|_| false);
        h.animator.start();
        h.animator.start();
        h.animator.start();
        assert!(h.armed());
        // still just one pending frame: firing it settles the loop
        h.advance(16.0);
        assert!(!h.armed());
    }

    #[test]
    fn stop_cancels_a_pending_frame() {
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        let h = Harness::new(move |_| {
            r.set(true);
            true
        });
        h.animator.start();
        h.animator.stop();
        h.advance(16.0);
        assert!(!ran.get());
        assert!(!h.animator.active());
    }

    #[test]
    fn a_stop_from_inside_tick_ends_the_loop_even_when_tick_wants_more() {
        let slot: Rc<RefCell<Option<Rc<Animator>>>> = Rc::new(RefCell::new(None));
        let sl = slot.clone();
        let h = Harness::build(|_| {
            Rc::new(move |_| {
                if let Some(a) = sl.borrow().as_ref() {
                    a.stop();
                }
                true
            })
        });
        // Re-home the animator behind an Rc so the tick can reach it.
        let Harness {
            animator,
            t,
            pending,
        } = h;
        let animator = Rc::new(animator);
        *slot.borrow_mut() = Some(animator.clone());
        animator.start();
        t.set(t.get() + 16.0);
        let task = pending.borrow_mut().take().unwrap();
        task();
        assert!(!animator.active());
        assert!(pending.borrow().is_none());
        *slot.borrow_mut() = None;
    }

    #[test]
    fn restarts_after_settling() {
        let h = Harness::new(|_| false);
        h.animator.start();
        h.advance(16.0);
        assert!(!h.animator.active());
        h.animator.start();
        assert!(h.animator.active());
    }

    #[test]
    fn ease_toward_approaches_the_target_monotonically_and_converges() {
        let mut v = 0.0;
        let mut seq = Vec::new();
        for _ in 0..40 {
            v = ease_toward(v, 100.0, 16.0, 70.0);
            seq.push(v);
        }
        for w in seq.windows(2) {
            assert!(w[1] >= w[0]);
        }
        let last = *seq.last().unwrap();
        assert!(last <= 100.0);
        assert!(last > 99.0);
    }

    #[test]
    fn ease_toward_is_frame_rate_independent() {
        let one = ease_toward(0.0, 100.0, 32.0, 70.0);
        let mut two = ease_toward(0.0, 100.0, 16.0, 70.0);
        two = ease_toward(two, 100.0, 16.0, 70.0);
        assert!((one - two).abs() < 0.5);
    }

    #[test]
    fn ease_toward_jumps_straight_to_the_target_when_tau_is_zero() {
        assert_eq!(ease_toward(5.0, 42.0, 16.0, 0.0), 42.0);
        assert_eq!(ease_toward(5.0, 42.0, 16.0, -3.0), 42.0);
    }

    #[test]
    fn ease_toward_one_time_constant_covers_about_63_percent() {
        let v = ease_toward(0.0, 100.0, 100.0, 100.0);
        assert!((v - 63.212_055_882_855_77).abs() < 1e-9);
        assert_eq!(ease_toward(7.0, 7.0, 16.0, 70.0), 7.0);
    }

    #[test]
    fn ease_setting_remembers_the_last_non_zero_duration_across_a_toggle() {
        let mut e = EaseSetting::new(70.0);
        assert_eq!(e.tau(), 70.0);
        e.set(200.0);
        e.toggle(false);
        assert!(!e.on());
        assert_eq!(e.tau(), 0.0);
        e.toggle(true);
        assert_eq!(e.tau(), 200.0); // the configured duration, not the default
        e.set(0.0);
        e.toggle(true);
        assert_eq!(e.tau(), 200.0); // set(0) is off, not a new remembered value
    }

    #[test]
    fn a_motion_that_ships_off_still_turns_on_at_its_default() {
        let mut e = EaseSetting::with_initial(90.0, 0.0);
        assert!(!e.on());
        e.toggle(true);
        assert_eq!(e.tau(), 90.0);
    }

    #[test]
    fn a_negative_duration_reads_as_off() {
        let mut e = EaseSetting::new(70.0);
        e.set(-5.0);
        assert!(!e.on());
        e.toggle(true);
        assert_eq!(e.tau(), 70.0);
    }
}
