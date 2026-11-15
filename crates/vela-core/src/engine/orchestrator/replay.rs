//! The bar-replay section of the orchestrator: everything named `replay*` and
//! its private helpers (forming bar, ticks, timers, `end_replay`,
//! `merge_history_into_replay`, `restart_bar_consumers`, ...).
//!
//! Shape notes:
//! - `replay_start` runs its synchronous prefix when it is CALLED (the original
//!   is an `async` function: a start with no history to wait for cuts
//!   synchronously, and `set_market` relies on that through
//!   `resume_carried_replay`). The body runs through `detach`, so it also runs
//!   to completion when the caller drops the returned future, as a promise's
//!   work does.
//! - The queue is a `VecDeque` (`State::replay_queue`); `State::replay_carrying`
//!   is shared by identity (`Rc::ptr_eq`), its `cancelled` flag is a `Cell`.
//! - The original compares `replayTickAbort` by identity to tell a request epoch
//!   from the next. The controller is only ever replaced after it was aborted
//!   (`drop_tick_fetches`), so "my epoch is current" is "my signal is not aborted".
//! - A tick source is a future-returning function; the original's synchronous
//!   source is a ready future here, polled once at call time (`request_ticks`),
//!   so it lands inside the call as the array does there. The original's
//!   `async` sources (a promise: `lower_timeframe_ticks`) are futures that
//!   yield a turn first, so they land a turn after the call, as there. A
//!   promise-returning source that happens to be ready is indistinguishable
//!   from a synchronous one and lands in the call.
//! - Every `await` of the original is `js_await` (see `runtime`); the `.then`
//!   callbacks stay plain awaits inside spawned tasks.

use std::task::{Context, Poll, Waker};

use super::*;
use crate::controls::replay_control::ReplayTickRequest;
use crate::options::MarketSwitch;
use crate::ports::scripting_engine::BarsChangeReason;
use crate::runtime::AbortSignal;

/// What `end_replay` does: `restore` puts the queued bars back and restarts the
/// consumers over the full history; `resume` restarts live updates and heals the
/// bars that closed while replaying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EndReplayOpts {
    pub restore: bool,
    pub resume: bool,
}

/// The depth a replay without a remembered depth falls back to.
const DEFAULT_DEPTH_BARS: usize = 500;

impl EngineOrchestrator {
    /// Enter replay, or seek when already replaying, keeping every bar opened
    /// at or before `opts.from` and queueing the rest. A `from` inside the
    /// loaded bars cuts at once, even while older history is still streaming in:
    /// what lands later joins the tape's head (see `merge_history_into_replay`)
    /// and leaves the cut alone. An older `from` waits for that load, then
    /// deepens if it still has to. Consumers restart over the cut exactly as on a
    /// market switch (their models describe bars that are now hidden) and the
    /// active chart-type engine is rebuilt without live data.
    pub fn replay_start(
        &self,
        opts: ReplayStartOptions,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        let result: ResolveOnce<Result<(), DataError>> = ResolveOnce::new();
        let out = result.clone();
        let body = Self::replay_start_body(self.clone(), opts);
        // The synchronous prefix (the guard, the cut when there is no history to
        // wait for) runs here; the rest runs detached.
        self.detach(Box::pin(async move {
            out.resolve(body.await);
        }));
        Box::pin(async move { result.wait().await })
    }

    fn replay_start_body(
        this: EngineOrchestrator,
        opts: ReplayStartOptions,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        Box::pin(async move {
            let first = this.inner.state.borrow().raw_bars.first().map(|b| b.time);
            let Some(first) = first else {
                this.warn("[vela] chart.replay.start() ignored — the chart has no bars yet");
                return Ok(());
            };
            let mut epoch = this.generation();
            if opts.from < first {
                js_await(this.history_complete()).await;
                if this.generation() != epoch || this.inner.state.borrow().raw_bars.is_empty() {
                    return Ok(()); // a market switch superseded the request
                }
            }
            let oldest = this.inner.state.borrow().raw_bars.first().map(|b| b.time);
            let Some(oldest) = oldest else {
                return Ok(());
            };
            if opts.from < oldest && this.can_heal() && this.inner.feed.supports_load_range() {
                // Deepen through the depth-only path. Bars per interval
                // overestimates on session-bound markets, which only over-fetches;
                // genesis clamps the rest.
                let (held, seek) = {
                    let st = this.inner.state.borrow();
                    (
                        st.raw_bars.len() + st.replay_queue.as_ref().map_or(0, VecDeque::len),
                        st.replay_queue.is_some(),
                    )
                };
                let span = (oldest - opts.from) as f64 / this.bar_interval_ms() as f64;
                let bars = held + span.ceil() as usize + 1;
                if seek {
                    // A seek, not an end.
                    this.end_replay(
                        None,
                        EndReplayOpts {
                            restore: true,
                            resume: false,
                        },
                    );
                }
                {
                    let mut st = this.inner.state.borrow_mut();
                    if st.replay_depth.is_none() {
                        st.replay_depth = Some(ReplayDepth {
                            bars: st.config.market.bars,
                        });
                    }
                }
                let deepened = this.set_market(MarketSwitch {
                    bars: Some(bars),
                    ..MarketSwitch::default()
                });
                // Flagged only once the deepening call has read its arguments: its
                // own depth change must not count as a switch abandoning the load.
                this.inner.state.borrow_mut().replay_loading = Some(ReplayLoading { seek });
                js_await(deepened).await?;
                epoch = this.generation();
                js_await(this.history_complete()).await;
                let mut st = this.inner.state.borrow_mut();
                if st.replay_loading.is_none() || st.generation != epoch || st.raw_bars.is_empty() {
                    return Ok(()); // cancelled or superseded
                }
                st.replay_loading = None;
            }
            this.settle_forming_bar();
            this.drop_tick_fetches();
            let tape: Vec<Ohlcv> = {
                let st = this.inner.state.borrow();
                let mut tape = st.raw_bars.clone();
                if let Some(queue) = &st.replay_queue {
                    tape.extend(queue.iter().copied());
                }
                tape
            };
            let mut cut = 0;
            while cut < tape.len() && tape[cut].time <= opts.from {
                cut += 1;
            }
            cut = cut.max(1);
            if cut >= tape.len() {
                this.warn(
                    "[vela] chart.replay.start() ignored — no bar opens after the requested time",
                );
                return Ok(());
            }
            let (was_playing, was_replaying) = {
                let st = this.inner.state.borrow();
                (st.replay_playing, st.replay_queue.is_some())
            };
            this.clear_replay_timer();
            if !was_replaying {
                this.stop_live();
            }
            let remaining = tape.len() - cut;
            this.inner.state.borrow_mut().replay_queue =
                Some(tape[cut..].iter().copied().collect());
            this.set_bar_series(tape[..cut].to_vec(), SetBarOpts::default());
            this.restart_bar_consumers(true);
            if was_playing {
                this.emit(ChartEvent::ReplayPause);
            }
            this.emit(ChartEvent::ReplayStart {
                cursor_time: tape[cut - 1].time,
                remaining,
            });
            this.prefetch_ticks();
            Ok(())
        })
    }

    /// Reveal the next queued bar through the live-bar path, or complete the bar
    /// forming tick by tick; the last one ends the replay.
    pub fn replay_step(&self) -> bool {
        if self.inner.state.borrow().replay_forming.is_some() {
            self.finish_forming_bar();
            return true;
        }
        let bar = {
            let mut st = self.inner.state.borrow_mut();
            let bar = st.replay_queue.as_mut().and_then(VecDeque::pop_front);
            if let Some(bar) = &bar {
                st.replay_tick_fetches.remove(&bar.time);
            }
            bar
        };
        let Some(bar) = bar else { return false };
        self.apply_bar(bar, true);
        self.complete_reveal(&bar);
        self.prefetch_ticks();
        true
    }

    /// Follow up a replay carried onto a new series of the same symbol (another
    /// timeframe or session). The cut asked for keeps the bars that CLOSED by the
    /// end of what was revealed: every finer bar of the last revealed one, never
    /// a coarser bar still open at that point (no look-ahead). Once it settles:
    /// playback resumes if it was on; if the replay did not come back (nothing
    /// left after that point, or a later switch took over), it ends as a market
    /// switch would. Returns whether the replay is back already: a start with no
    /// history to wait for cuts synchronously.
    pub(super) fn resume_carried_replay(
        &self,
        start: LocalBoxFuture<'static, Result<(), DataError>>,
        carry: &Rc<ReplayCarry>,
    ) -> bool {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.replay_queue.is_some()
                && st
                    .replay_carrying
                    .as_ref()
                    .is_some_and(|c| Rc::ptr_eq(c, carry))
            {
                st.replay_carrying = None; // back already
            }
        }
        let weak = self.weak();
        let carry = Rc::clone(carry);
        // A start that failed settles the carry like one that came back empty: the
        // original leaves it dangling (an unhandled rejection), which would keep
        // the replay "active" for good.
        self.spawn(Box::pin(async move {
            let _ = start.await;
            let Some(this) = upgrade(&weak) else { return };
            {
                let mut st = this.inner.state.borrow_mut();
                if st
                    .replay_carrying
                    .as_ref()
                    .is_some_and(|c| Rc::ptr_eq(c, &carry))
                {
                    st.replay_carrying = None;
                }
            }
            if carry.cancelled.get() {
                return; // stopped or superseded meanwhile: already announced
            }
            let replaying = this.inner.state.borrow().replay_queue.is_some();
            if !replaying {
                this.emit(ChartEvent::ReplayEnd {
                    reason: ReplayEndReason::Market,
                });
            } else if carry.playing {
                this.replay_play(None);
            }
        }));
        self.inner.state.borrow().replay_queue.is_some()
    }

    /// Reveal the next UPDATE: with ticks, the next tick of the forming bar, or
    /// the next bar opened at its first tick (once its ticks are in); otherwise
    /// the next whole bar.
    pub fn replay_step_update(&self) -> bool {
        let (has_queue, forming) = {
            let st = self.inner.state.borrow();
            (st.replay_queue.is_some(), st.replay_forming.is_some())
        };
        if !has_queue {
            return false;
        }
        if forming {
            self.apply_replay_ticks(1);
            return true;
        }
        let (bar, has_source) = {
            let st = self.inner.state.borrow();
            (
                st.replay_queue.as_ref().and_then(|q| q.front().copied()),
                st.replay_tick_source.is_some(),
            )
        };
        let Some(bar) = bar else { return false };
        let entry = has_source.then(|| self.request_ticks(&bar));
        let landed = entry.as_ref().and_then(|e| e.ticks.borrow().clone());
        if let Some(entry) = &entry
            && landed.is_none()
        {
            let epoch = self.tick_epoch();
            let wait = entry.done.wait();
            let weak = self.weak();
            // Timed playback, when on, opens the bar itself once the ticks land.
            self.spawn(Box::pin(async move {
                wait.await;
                let Some(this) = upgrade(&weak) else { return };
                let ready = {
                    let st = this.inner.state.borrow();
                    !st.replay_playing
                        && st.replay_forming.is_none()
                        && Self::epoch_is_current(epoch.as_ref(), &st)
                        && st
                            .replay_queue
                            .as_ref()
                            .and_then(|q| q.front())
                            .map(|b| b.time)
                            == Some(bar.time)
                };
                if ready {
                    this.replay_step_update();
                }
            }));
            return true;
        }
        match landed {
            Some(ticks) if !ticks.is_empty() => {
                self.open_forming_bar(ticks, Some(1));
                true
            }
            _ => self.replay_step(),
        }
    }

    /// The signal standing for the current tick-request epoch.
    fn tick_epoch(&self) -> Option<AbortSignal> {
        self.inner
            .state
            .borrow()
            .replay_tick_abort
            .as_ref()
            .map(AbortController::signal)
    }

    fn epoch_is_current(epoch: Option<&AbortSignal>, st: &State) -> bool {
        match epoch {
            Some(signal) => !signal.aborted(),
            None => st.replay_tick_abort.is_none(),
        }
    }

    /// Source of the intrabar prices for the bars replay reveals; `None` goes
    /// back to whole bars. A bar forming when the source changes completes at
    /// once.
    pub fn replay_set_ticks(&self, source: Option<ReplayTickSource>) {
        self.finish_forming_bar();
        self.drop_tick_fetches();
        self.inner.state.borrow_mut().replay_tick_source = source;
        self.prefetch_ticks();
    }

    /// Reveal one bar (one tick, with ticks) every `interval_ms` (default: the
    /// last interval used). A non-positive or non-finite interval is ignored
    /// with a warning.
    pub fn replay_play(&self, interval_ms: Option<f64>) {
        if self.inner.state.borrow().replay_queue.is_none() {
            return;
        }
        if let Some(interval) = interval_ms {
            if !(interval.is_finite() && interval > 0.0) {
                self.warn(&format!(
                    "[vela] chart.replay.play({interval}) ignored — the interval must be a positive number of ms"
                ));
                return;
            }
            self.inner.state.borrow_mut().replay_interval_ms = interval;
        }
        self.clear_replay_timer();
        let now = self.now();
        let (delay, interval) = {
            let mut st = self.inner.state.borrow_mut();
            st.replay_playing = true;
            let interval = st.replay_interval_ms;
            let delay = st.replay_forming.as_mut().map(|forming| {
                Self::pace_forming_bar(forming, interval, now);
                forming.delay_ms
            });
            (delay, interval)
        };
        self.schedule_replay_step(delay);
        self.emit(ChartEvent::ReplayPlay {
            interval_ms: interval,
        });
    }

    pub fn replay_pause(&self) {
        if !self.inner.state.borrow().replay_playing {
            return;
        }
        self.clear_replay_timer();
        self.emit(ChartEvent::ReplayPause);
    }

    /// Leave replay: the full history comes back and live updates resume. Also
    /// cancels a start still loading older history (the chart returns to its
    /// previous depth).
    pub fn replay_stop(&self) {
        let carrying = self.inner.state.borrow_mut().replay_carrying.take();
        if let Some(carrying) = carrying {
            // Carried over a switch and not back yet: it must not resume once the
            // bars land.
            carrying.cancelled.set(true);
            self.emit(ChartEvent::ReplayEnd {
                reason: ReplayEndReason::Stopped,
            });
        }
        let loading = self.inner.state.borrow().replay_loading.is_some();
        if loading {
            // A start still loading older history: shrinking back to the borrowed
            // depth supersedes the backfill, and that reload resumes live updates
            // itself.
            let depth = self.inner.state.borrow().replay_depth.clone();
            self.abandon_replay_load(ReplayEndReason::Stopped);
            // The switch's work is detached: dropping the waiter is `void`.
            drop(self.set_market(MarketSwitch {
                bars: Some(depth.and_then(|d| d.bars).unwrap_or(DEFAULT_DEPTH_BARS)),
                ..MarketSwitch::default()
            }));
            return;
        }
        self.end_replay(
            Some(ReplayEndReason::Stopped),
            EndReplayOpts {
                restore: true,
                resume: true,
            },
        );
    }

    /// Drop a start's pending history load, announcing the end of a replay that
    /// left silently to deepen (`seek`).
    pub(super) fn abandon_replay_load(&self, reason: ReplayEndReason) {
        let loading = {
            let mut st = self.inner.state.borrow_mut();
            let loading = st.replay_loading.take();
            if loading.is_some() {
                st.replay_depth = None;
            }
            loading
        };
        if let Some(loading) = loading
            && loading.seek
        {
            self.emit(ChartEvent::ReplayEnd { reason });
        }
    }

    pub fn replay_state(&self) -> ReplayState {
        let st = self.inner.state.borrow();
        let active = st.replay_queue.is_some();
        ReplayState {
            // A replay carried over a switch stays on while the new bars load.
            active: active || st.replay_carrying.is_some(),
            playing: st
                .replay_carrying
                .as_ref()
                .map_or(st.replay_playing, |c| c.playing),
            cursor_time: if active {
                st.raw_bars.last().map(|b| b.time)
            } else {
                None
            },
            remaining: st.replay_queue.as_ref().map_or(0, VecDeque::len),
            next_time: st.replay_forming.as_ref().map(|f| f.bar.time).or_else(|| {
                st.replay_queue
                    .as_ref()
                    .and_then(|q| q.front())
                    .map(|b| b.time)
            }),
            interval_ms: st.replay_interval_ms,
        }
    }

    pub fn replay_bounds(&self) -> Option<ReplayBounds> {
        let st = self.inner.state.borrow();
        let first = st.raw_bars.first()?;
        let last = st
            .replay_queue
            .as_ref()
            .and_then(|q| q.back())
            .or_else(|| st.raw_bars.last())?;
        Some(ReplayBounds {
            first: first.time,
            last: last.time,
        })
    }

    /// Arm the timer of the next timed-playback beat (`None`: one play
    /// interval).
    pub(super) fn schedule_replay_step(&self, delay_ms: Option<f64>) {
        let delay = delay_ms.unwrap_or_else(|| self.inner.state.borrow().replay_interval_ms);
        let weak = self.weak();
        let id = self.inner.platform.set_timeout(
            delay,
            Box::new(move || {
                let Some(this) = upgrade(&weak) else { return };
                this.inner.state.borrow_mut().replay_timer = None;
                this.replay_advance();
            }),
        );
        self.inner.state.borrow_mut().replay_timer = Some(id);
    }

    /// One beat of timed playback: the forming bar's next ticks, else the next
    /// bar, whole or opened from its ticks (waiting for them when they have not
    /// landed yet).
    pub(super) fn replay_advance(&self) {
        let (forming, interval) = {
            let st = self.inner.state.borrow();
            if !st.replay_playing || st.replay_queue.is_none() {
                return;
            }
            (st.replay_forming.clone(), st.replay_interval_ms)
        };
        if let Some(forming) = forming {
            // Every tick due by the clock: a timer that fired late (throttling, a
            // busy frame) catches up instead of stretching the bar past its
            // interval.
            let due = ((self.now() - forming.origin) / interval).floor() + 1.0;
            let count = (forming.batch as f64).max(due - forming.next as f64);
            self.apply_replay_ticks(count as usize);
            if self.inner.state.borrow().replay_playing {
                self.schedule_replay_step(Some(forming.delay_ms));
            }
            return;
        }
        let (bar, has_source) = {
            let st = self.inner.state.borrow();
            (
                st.replay_queue.as_ref().and_then(|q| q.front().copied()),
                st.replay_tick_source.is_some(),
            )
        };
        let Some(bar) = bar else { return };
        let entry = has_source.then(|| self.request_ticks(&bar));
        let landed = entry.as_ref().and_then(|e| e.ticks.borrow().clone());
        if let Some(entry) = &entry
            && landed.is_none()
        {
            let epoch = self.tick_epoch();
            let wait = entry.done.wait();
            let weak = self.weak();
            self.spawn(Box::pin(async move {
                wait.await;
                let Some(this) = upgrade(&weak) else { return };
                let ready = {
                    let st = this.inner.state.borrow();
                    st.replay_playing
                        && st.replay_timer.is_none()
                        && Self::epoch_is_current(epoch.as_ref(), &st)
                        && st
                            .replay_queue
                            .as_ref()
                            .and_then(|q| q.front())
                            .map(|b| b.time)
                            == Some(bar.time)
                };
                if ready {
                    this.replay_advance();
                }
            }));
            return;
        }
        match landed {
            Some(ticks) if !ticks.is_empty() => {
                self.open_forming_bar(ticks, None);
                let delay = {
                    let st = self.inner.state.borrow();
                    st.replay_playing
                        .then(|| st.replay_forming.as_ref().map(|f| f.delay_ms))
                };
                if let Some(delay) = delay {
                    self.schedule_replay_step(delay);
                }
            }
            _ => {
                if self.replay_step() && self.inner.state.borrow().replay_playing {
                    self.schedule_replay_step(None);
                }
            }
        }
    }

    /// Take the next queued bar off the tape and open it at its first tick (the
    /// first `count` ticks; `None`: one timed update's worth).
    pub(super) fn open_forming_bar(&self, ticks: Rc<[ReplayTick]>, count: Option<usize>) {
        let now = self.now();
        let batch = {
            let mut st = self.inner.state.borrow_mut();
            let Some(bar) = st.replay_queue.as_mut().and_then(VecDeque::pop_front) else {
                return;
            };
            let volume_each = ticks
                .iter()
                .all(|t| t.volume.is_none())
                .then(|| bar.volume.unwrap_or(0.0) / ticks.len() as f64);
            let interval = st.replay_interval_ms;
            let mut forming = ReplayForming {
                bar,
                ticks,
                next: 0,
                candle: None,
                volume_each,
                batch: 1,
                delay_ms: interval,
                origin: 0.0,
            };
            Self::pace_forming_bar(&mut forming, interval, now);
            let batch = forming.batch;
            st.replay_forming = Some(forming);
            batch
        };
        self.apply_replay_ticks(count.unwrap_or(batch));
        self.prefetch_ticks();
    }

    /// One tick per interval, batched when the interval is shorter than a frame;
    /// the clock is anchored on the ticks already applied (a resume or a pace
    /// change carries on from there, never jumps). Pure: the caller passes the
    /// play interval and the clock.
    pub(super) fn pace_forming_bar(
        forming: &mut ReplayForming,
        replay_interval_ms: f64,
        now_ms: f64,
    ) {
        forming.batch = (MIN_TICK_UPDATE_MS / replay_interval_ms).ceil().max(1.0) as usize;
        forming.delay_ms = replay_interval_ms * forming.batch as f64;
        forming.origin = now_ms - forming.next.saturating_sub(1) as f64 * replay_interval_ms;
    }

    /// Fold the next `count` ticks into the forming candle; the last one settles
    /// the bar.
    pub(super) fn apply_replay_ticks(&self, count: usize) {
        let (next, total, time, candle) = {
            let mut st = self.inner.state.borrow_mut();
            let Some(f) = st.replay_forming.as_mut() else {
                return;
            };
            let mut c = f.candle;
            let stop = f.ticks.len().min(f.next.saturating_add(count));
            while f.next < stop {
                let t = f.ticks[f.next];
                let v = f.volume_each.or(t.volume).unwrap_or(0.0);
                let hi = t.high.unwrap_or(t.price).max(t.price);
                let lo = t.low.unwrap_or(t.price).min(t.price);
                c = Some(match c {
                    Some(c) => Ohlcv {
                        high: c.high.max(hi),
                        low: c.low.min(lo),
                        close: t.price,
                        volume: Some(c.volume.unwrap_or(0.0) + v),
                        ..c
                    },
                    None => {
                        let open = t.open.unwrap_or(t.price);
                        Ohlcv {
                            time: f.bar.time,
                            open,
                            high: open.max(hi),
                            low: open.min(lo),
                            close: t.price,
                            volume: Some(v),
                        }
                    }
                });
                f.next += 1;
            }
            f.candle = c;
            (f.next, f.ticks.len(), f.bar.time, c)
        };
        if next >= total {
            self.finish_forming_bar();
            return;
        }
        let Some(candle) = candle else { return };
        self.apply_bar(candle, true);
        self.emit(ChartEvent::ReplayTick {
            cursor_time: time,
            index: next.saturating_sub(1),
            count: total,
        });
    }

    /// Settle the forming bar on its stored values and announce it revealed.
    pub(super) fn finish_forming_bar(&self) {
        let forming = {
            let mut st = self.inner.state.borrow_mut();
            let forming = st.replay_forming.take();
            if let Some(f) = &forming {
                st.replay_tick_fetches.remove(&f.bar.time);
            }
            forming
        };
        let Some(f) = forming else { return };
        self.apply_bar(f.bar, true);
        self.emit(ChartEvent::ReplayTick {
            cursor_time: f.bar.time,
            index: f.ticks.len().saturating_sub(1),
            count: f.ticks.len(),
        });
        self.complete_reveal(&f.bar);
    }

    /// Put the forming bar back in its stored form, silently: a seek or an end
    /// rebuilds from the tape, which must hold the stored bar.
    pub(super) fn settle_forming_bar(&self) {
        let forming = self.inner.state.borrow_mut().replay_forming.take();
        if let Some(f) = forming {
            self.apply_bar(f.bar, true);
        }
    }

    /// A history answer landing mid-replay (a progressive snapshot: the whole
    /// answer so far): the tape keeps its cut and its hidden queue; bars older
    /// than the tape join its head, bars newer than it join the queue's end.
    pub(super) fn merge_history_into_replay(&self, bars: &[Ohlcv]) {
        let joined = {
            let mut st = self.inner.state.borrow_mut();
            let Some(head) = st.raw_bars.first().map(|b| b.time) else {
                return;
            };
            let Some(queue) = st.replay_queue.as_ref() else {
                return;
            };
            let Some(tail) = queue
                .back()
                .map(|b| b.time)
                .or_else(|| st.raw_bars.last().map(|b| b.time))
            else {
                return;
            };
            let mut older = 0;
            while older < bars.len() && bars[older].time < head {
                older += 1;
            }
            let mut newer = bars.len();
            while newer > older && bars[newer - 1].time > tail {
                newer -= 1;
            }
            if newer < bars.len()
                && let Some(queue) = st.replay_queue.as_mut()
            {
                queue.extend(bars[newer..].iter().copied());
            }
            (older > 0).then(|| {
                let mut joined = bars[..older].to_vec();
                joined.extend(st.raw_bars.iter().copied());
                joined
            })
        };
        if let Some(joined) = joined {
            self.set_bar_series(
                joined,
                SetBarOpts {
                    preserve_view: true,
                    ..SetBarOpts::default()
                },
            );
            self.notify_sessions_bars(Some(BarsChangeReason::Backfill));
        }
    }

    /// A bar is fully on screen: announce it, and end the replay if it was the
    /// last.
    pub(super) fn complete_reveal(&self, bar: &Ohlcv) {
        let remaining = self
            .inner
            .state
            .borrow()
            .replay_queue
            .as_ref()
            .map(VecDeque::len);
        let Some(remaining) = remaining else { return };
        self.emit(ChartEvent::ReplayStep {
            cursor_time: bar.time,
            remaining,
        });
        // The original reads the same live queue after the event: a listener may
        // have moved the replay on.
        let empty = self
            .inner
            .state
            .borrow()
            .replay_queue
            .as_ref()
            .is_some_and(VecDeque::is_empty);
        if empty {
            self.end_replay(
                Some(ReplayEndReason::Finished),
                EndReplayOpts {
                    restore: true,
                    resume: true,
                },
            );
        }
    }

    /// Ask for the ticks of the next hidden bar ahead of time.
    pub(super) fn prefetch_ticks(&self) {
        let next = {
            let st = self.inner.state.borrow();
            if st.replay_tick_source.is_none() {
                return;
            }
            st.replay_queue.as_ref().and_then(|q| q.front().copied())
        };
        if let Some(next) = next {
            self.request_ticks(&next);
        }
    }

    /// The (shared) tick request of a queued bar: the source runs once per bar.
    pub(super) fn request_ticks(&self, bar: &Ohlcv) -> Rc<TickFetch> {
        let (source, signal, end) = {
            let mut st = self.inner.state.borrow_mut();
            if let Some(known) = st.replay_tick_fetches.get(&bar.time) {
                return Rc::clone(known);
            }
            let source = st.replay_tick_source.clone();
            let signal = st
                .replay_tick_abort
                .get_or_insert_with(AbortController::new)
                .signal();
            // `queue[queue.indexOf(bar) + 1]`: a bar not in the queue reads its head.
            let end = st.replay_queue.as_ref().and_then(|queue| {
                let after = queue
                    .iter()
                    .position(|b| b.time == bar.time)
                    .map_or(0, |i| i + 1);
                queue.get(after).map(|b| b.time)
            });
            (source, signal, end)
        };
        let end = end.unwrap_or_else(|| bar.time + self.bar_interval_ms());
        let entry = Rc::new(TickFetch {
            ticks: RefCell::new(None),
            done: ResolveOnce::new(),
        });
        let Some(source) = source else {
            // Only asked with a source in place; without one the bar reveals whole.
            *entry.ticks.borrow_mut() = Some(usable_ticks(Vec::new()));
            entry.done.resolve(());
            return entry;
        };
        let platform = Rc::clone(&self.inner.platform);
        let land = {
            let entry = Rc::clone(&entry);
            move |result: Result<Vec<ReplayTick>, DataError>, signal: &AbortSignal| {
                let ticks = match result {
                    Ok(ticks) => ticks,
                    Err(e) => {
                        if !signal.aborted() {
                            platform.warn(&format!(
                                "[vela] replay tick source failed — revealing the bar whole ({e})"
                            ));
                        }
                        Vec::new()
                    }
                };
                *entry.ticks.borrow_mut() = Some(usable_ticks(ticks));
                entry.done.resolve(());
            }
        };
        let mut pending = source(
            bar,
            ReplayTickRequest {
                end,
                signal: signal.clone(),
            },
        );
        // A ready answer lands inside the call, as the original's array does.
        let mut cx = Context::from_waker(Waker::noop());
        match pending.as_mut().poll(&mut cx) {
            Poll::Ready(result) => land(result, &signal),
            Poll::Pending => self.spawn(Box::pin(async move {
                let result = pending.await;
                land(result, &signal);
            })),
        }
        self.inner
            .state
            .borrow_mut()
            .replay_tick_fetches
            .insert(bar.time, Rc::clone(&entry));
        entry
    }

    /// Abort the tick requests in flight and forget the landed ones.
    pub(super) fn drop_tick_fetches(&self) {
        let abort = self.inner.state.borrow_mut().replay_tick_abort.take();
        if let Some(abort) = abort {
            abort.abort();
        }
        self.inner.state.borrow_mut().replay_tick_fetches.clear();
    }

    pub(super) fn clear_replay_timer(&self) {
        let timer = {
            let mut st = self.inner.state.borrow_mut();
            st.replay_playing = false;
            st.replay_timer.take()
        };
        if let Some(timer) = timer {
            self.inner.platform.clear_timeout(timer);
        }
    }

    /// Leave replay. `restore` puts the queued bars back and restarts the
    /// consumers over the full history (a market switch skips it: its load
    /// replaces everything anyway); `resume` restarts live updates and heals the
    /// bars that closed while replaying. A `None` reason leaves silently: a seek
    /// that re-enters right after.
    pub(super) fn end_replay(&self, reason: Option<ReplayEndReason>, opts: EndReplayOpts) {
        if self.inner.state.borrow().replay_queue.is_none() {
            return;
        }
        self.clear_replay_timer();
        if opts.restore {
            self.settle_forming_bar();
        } else {
            self.inner.state.borrow_mut().replay_forming = None;
        }
        self.drop_tick_fetches();
        let taken = self.inner.state.borrow_mut().replay_queue.take();
        // A listener reached by the settle may have ended the replay already.
        let Some(queue) = taken else { return };
        // A real end hands back the depth the replay borrowed; a seek keeps it for
        // the re-entry.
        let depth = if reason.is_some() {
            self.inner.state.borrow_mut().replay_depth.take()
        } else {
            None
        };
        if let Some(depth) = &depth {
            self.inner.state.borrow_mut().config.market.bars = depth.bars;
        }
        if opts.restore {
            let keep = depth
                .as_ref()
                .map_or(usize::MAX, |d| d.bars.unwrap_or(DEFAULT_DEPTH_BARS));
            let (put_back, live) = {
                let st = self.inner.state.borrow();
                (
                    !queue.is_empty() || st.raw_bars.len() > keep,
                    st.config.live,
                )
            };
            if put_back {
                let mut full = self.inner.state.borrow().raw_bars.clone();
                full.extend(queue.iter().copied());
                if full.len() > keep {
                    full.drain(..full.len() - keep);
                }
                self.set_bar_series(full, SetBarOpts::default());
                self.restart_bar_consumers(false);
            } else if live {
                // Nothing to put back, but natives and the chart-type engine run a
                // replay-time (non-live) form: rebuild them live.
                self.restart_native_indicators();
                self.restart_chart_type_engine();
            }
        }
        if opts.resume && self.inner.state.borrow().config.live {
            self.start_live();
            let last = self.inner.state.borrow().raw_bars.last().map(|b| b.time);
            if let Some(last) = last
                && self.can_heal()
            {
                self.detach(self.heal_gap(last));
            }
        }
        if let Some(reason) = reason {
            self.emit(ChartEvent::ReplayEnd { reason });
        }
    }

    /// The bars were replaced by a cut or a restore of the SAME market: every
    /// consumer re-runs. A cut (`blank`) also blanks the mounted models first:
    /// they still paint the hidden bars' values, which must not show until the
    /// re-runs land.
    pub(super) fn restart_bar_consumers(&self, blank: bool) {
        if blank {
            self.blank_indicator_visuals();
        }
        self.restart_native_indicators();
        self.restart_chart_type_engine();
        self.reexecute_indicators();
    }
}

/// A tick source's answer, kept to the ticks whose price (and optional fields)
/// are finite.
pub(super) fn usable_ticks(ticks: Vec<ReplayTick>) -> Rc<[ReplayTick]> {
    let optional = |n: Option<f64>| n.is_none_or(f64::is_finite);
    ticks
        .into_iter()
        .filter(|t| {
            t.price.is_finite()
                && optional(t.volume)
                && optional(t.high)
                && optional(t.low)
                && optional(t.open)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::orchestrator::test_support::{
        FakeSession, HOUR, Rig, SessionCall, T0, to_symbol,
    };
    use crate::events::types::ChartEventName;
    use crate::model::Millis;

    /// The symbol's whole universe: `total` hourly bars, close = index (so a bar is
    /// identifiable).
    fn universe(total: usize) -> Vec<Ohlcv> {
        (0..total)
            .map(|i| {
                let c = i as f64;
                Ohlcv::new(T0 + i as i64 * HOUR, c, c + 1.0, c - 1.0, c, Some(1.0))
            })
            .collect()
    }

    /// A loaded chart serving the NEWEST `bars` of a `total`-bar universe.
    fn replay_rig(total: usize, bars: usize, live: bool) -> Rig {
        let rig = Rig::builder()
            .bars(bars)
            .live(live)
            .feed(move |f| f.set_series("AAA", universe(total)))
            .build();
        rig.loaded();
        rig
    }

    fn start_at(rig: &Rig, from: Millis) -> Result<(), DataError> {
        let started = rig.chart.replay_start(ReplayStartOptions { from });
        rig.block_on(started)
    }

    /// The bars on screen: the chart's view, which is what the renderer was handed.
    fn shown(rig: &Rig) -> Vec<Ohlcv> {
        rig.view()
    }

    fn last_shown(rig: &Rig) -> Ohlcv {
        *rig.view().last().expect("a bar")
    }

    fn closes(bars: &[Ohlcv]) -> Vec<f64> {
        bars.iter().map(|b| b.close).collect()
    }

    /// (live subscriptions made, unsubscribes) for the chart's symbol.
    fn live_counts(rig: &Rig) -> (usize, usize) {
        let (subs, unsubs) = rig.feed.subscriptions();
        (
            subs.get("AAA").copied().unwrap_or(0),
            unsubs.get("AAA").copied().unwrap_or(0),
        )
    }

    /// The replay events so far, one short string each (bars as their index).
    fn replay_events(rig: &Rig) -> Vec<String> {
        let at = |t: Millis| (t - T0) / HOUR;
        rig.log
            .all()
            .iter()
            .filter_map(|e| match e {
                ChartEvent::ReplayStart {
                    cursor_time,
                    remaining,
                } => Some(format!("start:{}:{remaining}", at(*cursor_time))),
                ChartEvent::ReplayStep {
                    cursor_time,
                    remaining,
                } => Some(format!("step:{}:{remaining}", at(*cursor_time))),
                ChartEvent::ReplayTick {
                    cursor_time,
                    index,
                    count,
                } => Some(format!("tick:{}:{index}/{count}", at(*cursor_time))),
                ChartEvent::ReplayPlay { interval_ms } => Some(format!("play:{interval_ms}")),
                ChartEvent::ReplayPause => Some("pause".to_string()),
                ChartEvent::ReplayEnd { reason } => Some(format!("end:{reason:?}")),
                _ => None,
            })
            .collect()
    }

    fn ends(rig: &Rig) -> Vec<ReplayEndReason> {
        rig.log
            .of(ChartEventName::ReplayEnd)
            .iter()
            .filter_map(|e| match e {
                ChartEvent::ReplayEnd { reason } => Some(*reason),
                _ => None,
            })
            .collect()
    }

    fn market_bars(rig: &Rig) -> Option<usize> {
        rig.chart.market_snapshot().bars
    }

    /// The ticks `i + 0.5`, `i - 0.5`, `i` for the bar whose close is `i`.
    fn three_ticks() -> ReplayTickSource {
        Rc::new(|bar, _| {
            let c = bar.close;
            Box::pin(std::future::ready(Ok(vec![
                ReplayTick::at(c + 0.5),
                ReplayTick::at(c - 0.5),
                ReplayTick::at(c),
            ])))
        })
    }

    fn state_of(rig: &Rig) -> ReplayState {
        rig.chart.replay_state()
    }

    // ── start, step, play, stop ──

    #[test]
    fn start_cuts_the_history_at_from_pauses_live_and_reports_the_queue() {
        let rig = replay_rig(200, 100, true);
        assert_eq!(live_counts(&rig), (1, 0));
        let on_screen = shown(&rig); // newest 100 of 200: closes 100..199
        let from = on_screen[39].time;

        start_at(&rig, from).expect("start");

        assert_eq!(shown(&rig).len(), 40);
        assert_eq!(last_shown(&rig).time, from);
        assert_eq!(live_counts(&rig), (1, 1), "live paused");
        assert_eq!(replay_events(&rig), ["start:139:60"]);
        assert_eq!(
            state_of(&rig),
            ReplayState {
                active: true,
                playing: false,
                cursor_time: Some(from),
                remaining: 60,
                next_time: Some(from + HOUR),
                interval_ms: 1000.0,
            }
        );
    }

    #[test]
    fn a_start_inside_the_loaded_bars_cuts_inside_the_call_and_survives_a_dropped_future() {
        let rig = replay_rig(200, 100, false);
        let from = shown(&rig)[9].time;
        // Not polled, and dropped at once: the cut happened at call time.
        drop(rig.chart.replay_start(ReplayStartOptions { from }));
        assert!(state_of(&rig).active);
        assert_eq!(shown(&rig).len(), 10);

        // An older start waits on a load: it must finish even if nobody awaits it.
        let older = shown(&rig)[0].time - 5 * HOUR;
        drop(rig.chart.replay_start(ReplayStartOptions { from: older }));
        rig.settle();
        assert!(state_of(&rig).active);
        assert_eq!(state_of(&rig).cursor_time, Some(older));
    }

    #[test]
    fn a_chart_with_no_bars_warns_and_does_not_start() {
        let rig = Rig::builder().feed(|f| f.gate_loads(true)).bars(10).build();
        start_at(&rig, T0).expect("ignored");
        assert!(!state_of(&rig).active);
        assert!(
            rig.platform
                .warnings()
                .iter()
                .any(|w| w.contains("the chart has no bars yet"))
        );
    }

    #[test]
    fn step_reveals_the_next_bar_through_the_live_bar_path() {
        let rig = replay_rig(200, 100, true);
        let from = shown(&rig)[49].time;
        start_at(&rig, from).expect("start");
        rig.settle();
        let session = rig.attach_session("probe", "// probe");
        let notified = session.notifies().len();
        rig.renderer.updated_bars.borrow_mut().clear();
        rig.log.clear();

        assert!(rig.chart.replay_step());

        assert_eq!(closes(&rig.renderer.updated_bars.borrow()), [150.0]);
        let bar_events: Vec<f64> = rig
            .log
            .of(ChartEventName::Bar)
            .iter()
            .filter_map(|e| match e {
                ChartEvent::Bar(b) => Some(b.close),
                _ => None,
            })
            .collect();
        assert_eq!(bar_events, [150.0]);
        assert_eq!(replay_events(&rig), ["step:150:49"]);
        assert_eq!(session.notifies().len(), notified + 1);
        assert_eq!(state_of(&rig).cursor_time, Some(from + HOUR));
    }

    #[test]
    fn play_reveals_one_bar_per_interval_pause_stops_and_play_again_changes_the_pace() {
        let rig = replay_rig(200, 100, false);
        let from = shown(&rig)[9].time;
        start_at(&rig, from).expect("start");
        rig.log.clear();
        rig.renderer.updated_bars.borrow_mut().clear();

        rig.chart.replay_play(Some(15.0));
        assert!(state_of(&rig).playing);
        rig.advance(100.0);
        rig.chart.replay_pause();
        let revealed = rig.renderer.updated_bars.borrow().len();
        assert_eq!(revealed, 6, "one bar per 15 ms over 100 ms");
        assert!(!state_of(&rig).playing);
        rig.advance(60.0);
        assert_eq!(rig.renderer.updated_bars.borrow().len(), revealed);

        rig.log.clear();
        rig.chart.replay_play(Some(10.0));
        rig.chart.replay_play(Some(20.0)); // a pace change while playing
        assert_eq!(state_of(&rig).interval_ms, 20.0);
        rig.chart.replay_pause();
        assert_eq!(replay_events(&rig), ["play:10", "play:20", "pause"]);
        // Nothing left running.
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    #[test]
    fn a_bad_interval_is_ignored_with_a_warning_and_changes_nothing() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[9].time).expect("start");
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            rig.chart.replay_play(Some(bad));
        }
        assert_eq!(rig.platform.warnings().len(), 4);
        assert!(!state_of(&rig).playing);
        assert_eq!(state_of(&rig).interval_ms, 1000.0);
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    #[test]
    fn play_and_pause_outside_a_replay_do_nothing() {
        let rig = replay_rig(200, 100, false);
        rig.chart.replay_play(Some(10.0));
        rig.chart.replay_pause();
        assert!(!rig.chart.replay_step());
        assert!(!rig.chart.replay_step_update());
        assert!(replay_events(&rig).is_empty());
        assert_eq!(state_of(&rig), ReplayState::off());
    }

    #[test]
    fn stop_restores_the_full_history_resumes_live_and_heals_the_gap() {
        let rig = replay_rig(200, 100, true);
        rig.attach_session("probe", "// probe");
        start_at(&rig, shown(&rig)[29].time).expect("start");
        rig.chart.replay_step();
        rig.log.clear();
        let ranges_before = rig.feed.ranges().len();

        rig.chart.replay_stop();
        rig.settle();

        let full = shown(&rig);
        assert_eq!(full.len(), 100);
        assert_eq!(full[99].close, 199.0);
        assert_eq!(live_counts(&rig), (2, 1), "live resumed");
        let ranges = rig.feed.ranges();
        assert_eq!(ranges.len(), ranges_before + 1);
        assert_eq!(
            ranges.last().and_then(|r| r.from),
            Some(full[99].time),
            "healed from the newest known bar"
        );
        assert_eq!(replay_events(&rig), ["end:Stopped"]);
        let state = state_of(&rig);
        assert!(!state.active && !state.playing);
        assert_eq!((state.cursor_time, state.remaining), (None, 0));
        assert!(!rig.chart.replay_step());
    }

    #[test]
    fn revealing_the_last_bar_ends_the_replay_as_finished_and_resumes_live() {
        let rig = replay_rig(200, 100, true);
        start_at(&rig, shown(&rig)[97].time).expect("start");
        rig.log.clear();

        assert!(rig.chart.replay_step());
        assert!(ends(&rig).is_empty());
        assert!(rig.chart.replay_step());

        assert_eq!(ends(&rig), [ReplayEndReason::Finished]);
        assert_eq!(&closes(&shown(&rig))[97..], [197.0, 198.0, 199.0]);
        assert_eq!(live_counts(&rig), (2, 1));
        assert!(!state_of(&rig).active);
    }

    #[test]
    fn seeking_while_active_rewinds_over_the_whole_tape_without_ending_the_replay() {
        let rig = replay_rig(200, 100, false);
        let full = shown(&rig);
        start_at(&rig, full[59].time).expect("start");
        rig.chart.replay_play(Some(1000.0));
        rig.log.clear();

        start_at(&rig, full[19].time).expect("backward");
        assert_eq!(shown(&rig).len(), 20);
        start_at(&rig, full[79].time).expect("forward, past the old cursor");
        assert_eq!(shown(&rig).len(), 80);

        assert_eq!(
            replay_events(&rig),
            ["pause", "start:119:80", "start:179:20"]
        );
        assert!(!state_of(&rig).playing);
    }

    #[test]
    fn a_from_older_than_the_loaded_history_deepens_it_first() {
        let rig = replay_rig(300, 100, false);
        let oldest = shown(&rig)[0].time; // close 200
        let ranges = rig.feed.ranges().len();

        start_at(&rig, oldest - 50 * HOUR).expect("start");

        assert!(rig.feed.ranges().len() > ranges);
        assert_eq!(last_shown(&rig).close, 150.0);
        assert_eq!(state_of(&rig).remaining, 49 + 100);
    }

    #[test]
    fn a_from_with_no_bar_after_it_is_ignored_with_a_warning() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[99].time).expect("ignored");
        assert!(
            rig.platform
                .warnings()
                .iter()
                .any(|w| w.contains("no bar opens after the requested time"))
        );
        assert!(!state_of(&rig).active);
        assert_eq!(shown(&rig).len(), 100);
        assert!(replay_events(&rig).is_empty());
    }

    #[test]
    fn bounds_span_the_whole_walkable_history_hidden_bars_included() {
        let rig = Rig::builder()
            .bars(100)
            .feed(|f| {
                f.set_series("AAA", universe(200));
                f.gate_loads(true);
            })
            .build();
        assert_eq!(rig.chart.replay_bounds(), None);
        rig.feed.gate_loads(false);
        rig.feed.release();
        rig.loaded();
        let full = shown(&rig);
        let bounds = ReplayBounds {
            first: full[0].time,
            last: full[99].time,
        };
        assert_eq!(rig.chart.replay_bounds(), Some(bounds));
        start_at(&rig, full[40].time).expect("start");
        assert_eq!(rig.chart.replay_bounds(), Some(bounds));
    }

    // ── depth borrowed by a start ──

    #[test]
    fn a_replay_that_deepened_the_history_hands_the_depth_back_when_it_ends() {
        let rig = replay_rig(300, 100, false);
        start_at(&rig, shown(&rig)[0].time - 50 * HOUR).expect("start");
        assert!(market_bars(&rig).is_some_and(|b| b > 100));

        rig.chart.replay_stop();
        rig.settle();

        assert_eq!(market_bars(&rig), Some(100));
        let full = shown(&rig);
        assert_eq!(full.len(), 100);
        assert_eq!(full[99].close, 299.0);
    }

    #[test]
    fn stop_cancels_a_start_still_loading_older_history_and_hands_the_depth_back() {
        let rig = replay_rig(300, 100, true);
        rig.feed.gate_ranges(true); // the old history never arrives
        let done = Rc::new(Cell::new(None));
        {
            let (started, done) = (
                rig.chart.replay_start(ReplayStartOptions {
                    from: shown(&rig)[0].time - 50 * HOUR,
                }),
                Rc::clone(&done),
            );
            rig.platform.spawn(Box::pin(async move {
                done.set(Some(started.await.is_ok()));
            }));
        }
        rig.settle();
        assert!(market_bars(&rig).is_some_and(|b| b > 100));
        assert_eq!(done.get(), None, "still waiting on the history");
        rig.log.clear();

        rig.chart.replay_stop();
        rig.feed.gate_ranges(false);
        rig.feed.release();
        rig.settle();

        assert_eq!(done.get(), Some(true), "released, not left hanging");
        assert!(!state_of(&rig).active);
        assert_eq!(market_bars(&rig), Some(100));
        assert_eq!(shown(&rig).len(), 100);
        assert!(ends(&rig).is_empty(), "no replay had started");
        let (subs, unsubs) = live_counts(&rig);
        assert_eq!(subs - unsubs, 1, "live updates run, exactly once");
    }

    #[test]
    fn a_market_switch_during_a_starts_history_load_abandons_it_at_the_original_depth() {
        let rig = replay_rig(300, 100, false);
        rig.feed.set_series("BBB", universe(300));
        rig.feed.gate_ranges(true);
        let started = rig.chart.replay_start(ReplayStartOptions {
            from: shown(&rig)[0].time - 50 * HOUR,
        });
        rig.platform.spawn(Box::pin(async move {
            let _ = started.await;
        }));
        rig.settle();
        rig.feed.gate_ranges(false);

        rig.switch(to_symbol("BBB")).expect("switch");
        rig.feed.release();
        rig.settle();

        assert!(!state_of(&rig).active);
        assert_eq!(market_bars(&rig), Some(100));
        assert_eq!(shown(&rig).len(), 100);
    }

    #[test]
    fn a_seek_that_deepens_leaves_silently_and_announces_nothing_until_it_restarts() {
        let rig = replay_rig(300, 100, false);
        let first = shown(&rig)[0].time;
        start_at(&rig, first + 10 * HOUR).expect("start");
        rig.log.clear();

        start_at(&rig, first - 20 * HOUR).expect("seek, deeper");

        // No end between the two starts: a seek, not a stop.
        assert_eq!(replay_events(&rig).len(), 1);
        assert!(replay_events(&rig)[0].starts_with("start:"));
        assert!(state_of(&rig).active);
        assert_eq!(state_of(&rig).cursor_time, Some(first - 20 * HOUR));
    }

    #[test]
    fn stopping_a_seek_that_is_still_loading_announces_the_end_of_the_replay_it_left() {
        let rig = replay_rig(300, 100, false);
        let first = shown(&rig)[0].time;
        start_at(&rig, first + 10 * HOUR).expect("start");
        rig.feed.gate_ranges(true);
        let started = rig.chart.replay_start(ReplayStartOptions {
            from: first - 20 * HOUR,
        });
        rig.platform.spawn(Box::pin(async move {
            let _ = started.await;
        }));
        rig.settle();
        rig.log.clear();

        rig.chart.replay_stop();
        rig.feed.gate_ranges(false);
        rig.feed.release();
        rig.settle();

        assert_eq!(ends(&rig), [ReplayEndReason::Stopped]);
        assert!(!state_of(&rig).active);
    }

    // ── progressive loads ──

    #[test]
    fn a_start_inside_a_converging_progressive_load_cuts_at_once_and_keeps_its_cut() {
        let all = universe(300);
        let rig = Rig::builder()
            .bars(300)
            .progressive(true)
            .feed({
                let all = all.clone();
                move |f| f.set_series("AAA", all)
            })
            .build();
        rig.feed.emit_batch(all[200..].to_vec()); // the newest 100 paint first
        rig.settle();
        assert_eq!(shown(&rig).len(), 100);

        let from = all[249].time;
        let started = Rc::new(Cell::new(false));
        {
            let (fut, started) = (
                rig.chart.replay_start(ReplayStartOptions { from }),
                Rc::clone(&started),
            );
            rig.platform.spawn(Box::pin(async move {
                let _ = fut.await;
                started.set(true);
            }));
        }
        rig.settle();
        assert!(started.get(), "no wait on the history still converging");
        let state = state_of(&rig);
        assert_eq!((state.cursor_time, state.remaining), (Some(from), 50));

        // A deeper snapshot: 100 older bars join the head.
        rig.feed.emit_batch(all[100..].to_vec());
        assert_eq!(shown(&rig).len(), 150);
        assert_eq!(shown(&rig)[0].time, all[100].time);
        let state = state_of(&rig);
        assert_eq!((state.cursor_time, state.remaining), (Some(from), 50));
        let backfill = rig.renderer.last_bars_call().expect("a set_bars call");
        assert!(backfill.preserve_view, "the view is kept");

        // The final answer: the rest of the head, and one bar past the tape.
        let mut newer = all[299];
        newer.time += HOUR;
        newer.close = 300.0;
        let mut finalized = all.clone();
        finalized.push(newer);
        rig.feed.finish_progressive(Ok(Some(finalized)));
        rig.settle();
        assert_eq!(shown(&rig).len(), 250);
        let state = state_of(&rig);
        assert_eq!((state.cursor_time, state.remaining), (Some(from), 51));
        assert_eq!(
            rig.chart.replay_bounds(),
            Some(ReplayBounds {
                first: all[0].time,
                last: newer.time
            })
        );
    }

    #[test]
    fn merging_history_notifies_the_sessions_of_the_backfill_and_ignores_a_chart_not_replaying() {
        let rig = replay_rig(200, 100, false);
        let all = universe(200);
        // Not replaying: nothing to merge into.
        rig.chart.merge_history_into_replay(&all);
        assert_eq!(shown(&rig).len(), 100);

        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.settle();
        let session = rig.attach_session("probe", "// probe");
        let before = session.notifies().len();
        rig.chart.merge_history_into_replay(&all[50..]);
        assert_eq!(shown(&rig).len(), 100, "the cut keeps its size");
        assert_eq!(
            session.notifies().len(),
            before + 1,
            "bars older than the head joined it"
        );
        assert_eq!(
            session.notifies().last().copied().flatten(),
            Some(BarsChangeReason::Backfill)
        );
    }

    // ── market switches ──

    #[test]
    fn a_symbol_switch_ends_the_replay_as_a_market_end() {
        let rig = replay_rig(200, 100, true);
        rig.feed.set_series("BBB", universe(200));
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.chart.replay_play(Some(1000.0));
        rig.log.clear();

        rig.switch(to_symbol("BBB")).expect("switch");
        rig.settle();

        assert_eq!(ends(&rig), [ReplayEndReason::Market]);
        let state = state_of(&rig);
        assert!(!state.active && !state.playing);
        assert_eq!(shown(&rig).len(), 100);
        assert_eq!(
            rig.feed.active_subscribers(),
            1,
            "the switch's own live restart"
        );
        assert_eq!(rig.platform.pending_timers(), 0, "the play timer is gone");
    }

    #[test]
    fn a_timeframe_switch_keeps_the_replay_same_point_still_playing_live_still_off() {
        let rig = replay_rig(200, 100, true);
        let cursor = shown(&rig)[49].time;
        start_at(&rig, cursor).expect("start");
        rig.chart.replay_play(Some(10_000.0));
        rig.log.clear();
        let subs = live_counts(&rig).0;

        rig.switch(MarketSwitch {
            timeframe: Some("30".to_string()),
            ..MarketSwitch::default()
        })
        .expect("switch");
        rig.settle();

        let names = rig.log.names();
        assert!(!names.contains(&"replay:end"), "{names:?}");
        assert!(names.contains(&"replay:start") && names.contains(&"market:changed"));
        let state = state_of(&rig);
        assert!(state.active && state.playing);
        assert_eq!(state.cursor_time, Some(cursor));
        assert_eq!(
            live_counts(&rig).0,
            subs,
            "no live subscription while replaying"
        );
        rig.chart.replay_stop();
    }

    #[test]
    fn the_replay_reads_active_all_through_a_timeframe_switch() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.feed.gate_loads(true);

        let switching = rig.chart.set_market(MarketSwitch {
            timeframe: Some("30".to_string()),
            ..MarketSwitch::default()
        });
        assert!(state_of(&rig).active, "the new bars are still on their way");
        assert_eq!(state_of(&rig).cursor_time, None);
        rig.feed.gate_loads(false);
        rig.feed.release();
        rig.block_on(switching).expect("switch");
        assert!(state_of(&rig).active);
        assert!(state_of(&rig).cursor_time.is_some());
    }

    #[test]
    fn stop_while_a_replay_is_carried_over_a_switch_cancels_it_for_good() {
        let rig = replay_rig(200, 100, true);
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.feed.gate_loads(true);
        let subs = live_counts(&rig).0;
        rig.log.clear();

        let switching = rig.chart.set_market(MarketSwitch {
            timeframe: Some("30".to_string()),
            ..MarketSwitch::default()
        });
        rig.chart.replay_stop();
        rig.feed.gate_loads(false);
        rig.feed.release();
        rig.block_on(switching).expect("switch");
        rig.settle();

        assert!(!state_of(&rig).active);
        assert_eq!(ends(&rig), [ReplayEndReason::Stopped]);
        assert_eq!(live_counts(&rig).0, subs + 1, "back to live");
        assert_eq!(shown(&rig).len(), 100);
    }

    #[test]
    fn a_symbol_switch_landing_while_a_replay_is_carried_over_ends_it() {
        let rig = replay_rig(200, 100, false);
        rig.feed.set_series("BBB", universe(200));
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.feed.gate_loads(true);
        rig.log.clear();

        drop(rig.chart.set_market(MarketSwitch {
            timeframe: Some("30".to_string()),
            ..MarketSwitch::default()
        }));
        let to_bbb = rig.chart.set_market(MarketSwitch {
            symbol: Some("BBB".to_string()),
            ..MarketSwitch::default()
        });
        rig.feed.gate_loads(false);
        rig.feed.release();
        rig.block_on(to_bbb).expect("switch");
        rig.settle();

        assert!(!state_of(&rig).active);
        assert_eq!(ends(&rig), [ReplayEndReason::Market]);
    }

    #[test]
    fn switching_to_a_coarser_timeframe_hides_the_bar_still_open_at_the_replay_point() {
        let rig = replay_rig(200, 100, false);
        let cursor = shown(&rig)[49].time;
        start_at(&rig, cursor).expect("start");

        rig.switch(MarketSwitch {
            timeframe: Some("240".to_string()),
            ..MarketSwitch::default()
        })
        .expect("switch");
        rig.settle();

        let state = state_of(&rig);
        assert!(state.active);
        assert_eq!(state.cursor_time, Some(cursor - 3 * HOUR));
    }

    #[test]
    fn a_carried_replay_point_older_than_the_new_series_deepens_it_then_resumes() {
        let rig = replay_rig(300, 100, false);
        let cursor = shown(&rig)[0].time;
        start_at(&rig, cursor).expect("start");
        rig.log.clear();
        let ranges = rig.feed.ranges().len();

        let switching = rig.chart.set_market(MarketSwitch {
            timeframe: Some("120".to_string()),
            ..MarketSwitch::default()
        });
        // The point sits 1h before the loaded head: on all along while older bars load.
        assert!(state_of(&rig).active);
        rig.block_on(switching).expect("switch");
        rig.settle();

        assert!(rig.feed.ranges().len() > ranges, "deepened");
        assert_eq!(state_of(&rig).cursor_time, Some(cursor - HOUR));
        assert!(ends(&rig).is_empty());
    }

    #[test]
    fn a_carried_replay_that_cannot_come_back_ends_as_a_market_switch_would() {
        let rig = replay_rig(200, 100, false);
        // Revealed to the very last bar but one; a coarser frame leaves nothing after the cut.
        start_at(&rig, shown(&rig)[98].time).expect("start");
        rig.log.clear();
        // The new series holds only bars up to the cursor.
        let cut: Vec<Ohlcv> = universe(200)[..=198].to_vec();
        rig.feed.set_series("AAA", cut);

        rig.switch(MarketSwitch {
            timeframe: Some("30".to_string()),
            ..MarketSwitch::default()
        })
        .expect("switch");
        rig.settle();

        assert_eq!(ends(&rig), [ReplayEndReason::Market]);
        assert!(!state_of(&rig).active);
    }

    // ── chart-type engine and sessions ──

    #[test]
    fn a_data_engine_type_is_rebuilt_without_live_data_and_follows_each_revealed_bar() {
        use crate::chart_types::{ChartTypeDefinition, SeriesDataEngine, SeriesDataEngineHost};
        use std::sync::Arc;

        thread_local! {
            static HOSTS: RefCell<Vec<Arc<dyn SeriesDataEngineHost>>> =
                const { RefCell::new(Vec::new()) };
            static ON_BARS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        }
        struct Probe {
            host: Option<Arc<dyn SeriesDataEngineHost>>,
        }
        impl SeriesDataEngine for Probe {
            fn start(&mut self, host: Arc<dyn SeriesDataEngineHost>) {
                HOSTS.with(|h| h.borrow_mut().push(Arc::clone(&host)));
                self.host = Some(host);
            }
            fn suspend(&mut self) {}
            fn resume(&mut self) {}
            fn stop(&mut self) {}
            fn on_bars(&mut self) {
                let n = self.host.as_ref().map_or(0, |h| h.bars().len());
                ON_BARS.with(|b| b.borrow_mut().push(n));
            }
        }
        let live_flags = || HOSTS.with(|h| h.borrow().iter().map(|x| x.live()).collect::<Vec<_>>());

        let def = ChartTypeDefinition {
            data_engine: Some(Arc::new(|| {
                Box::new(Probe { host: None }) as Box<dyn SeriesDataEngine>
            })),
            ..ChartTypeDefinition::new("replay-probe")
        };
        let rig = Rig::builder()
            .bars(100)
            .live(true)
            .price_style("replay-probe")
            .feed(|f| f.set_series("AAA", universe(200)))
            .registries(|r| r.chart_types.borrow_mut().register(def))
            .build();
        rig.loaded();
        assert_eq!(live_flags(), [true]);

        start_at(&rig, shown(&rig)[39].time).expect("start");
        rig.settle();
        assert_eq!(live_flags(), [true, false]);
        assert_eq!(
            HOSTS.with(|h| h.borrow()[1].bars().len()),
            40,
            "never sees past the cursor"
        );

        ON_BARS.with(|b| b.borrow_mut().clear());
        rig.chart.replay_step();
        assert_eq!(ON_BARS.with(|b| b.borrow().clone()), [41]);

        rig.chart.replay_stop();
        rig.settle();
        assert_eq!(live_flags(), [true, false, true]);
    }

    #[test]
    fn a_session_is_told_of_every_revealed_bar_and_of_each_tick_of_a_forming_one() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.settle();
        let session: Rc<FakeSession> = rig.attach_session("probe", "// probe");
        let before = session.count(|c| matches!(c, SessionCall::NotifyBars(_)));
        rig.chart.replay_set_ticks(Some(three_ticks()));
        rig.chart.replay_step_update();
        rig.chart.replay_step_update();
        rig.chart.replay_step_update();
        assert_eq!(
            session.count(|c| matches!(c, SessionCall::NotifyBars(_))),
            before + 3
        );
    }

    // ── tick replay ──

    fn tick_rig() -> (Rig, Millis) {
        let rig = replay_rig(200, 100, false);
        let from = shown(&rig)[49].time; // next hidden bar: close 150, high 151, low 149, volume 1
        start_at(&rig, from).expect("start");
        rig.log.clear();
        rig.renderer.updated_bars.borrow_mut().clear();
        (rig, from)
    }

    fn updated(rig: &Rig) -> Vec<Ohlcv> {
        rig.renderer.updated_bars.borrow().clone()
    }

    #[test]
    fn playing_a_bar_walks_the_forming_candle_through_its_ticks_then_settles_on_the_stored_bar() {
        let (rig, from) = tick_rig();
        rig.chart.replay_set_ticks(Some(three_ticks()));

        // The interval is per TICK: the bar opens one interval in, then one tick per 60 ms.
        rig.chart.replay_play(Some(60.0));
        rig.advance(60.0);
        assert_eq!(
            updated(&rig),
            [Ohlcv::new(
                from + HOUR,
                150.5,
                150.5,
                150.5,
                150.5,
                Some(1.0 / 3.0)
            )]
        );
        rig.advance(60.0);
        let second = updated(&rig)[1];
        assert_eq!(
            (second.open, second.high, second.low, second.close),
            (150.5, 150.5, 149.5, 149.5)
        );
        rig.advance(59.0);
        assert_eq!(updated(&rig).len(), 2);
        rig.advance(1.0);
        rig.chart.replay_pause();

        assert_eq!(
            updated(&rig)[2],
            Ohlcv::new(from + HOUR, 150.0, 151.0, 149.0, 150.0, Some(1.0))
        );
        assert_eq!(
            replay_events(&rig),
            [
                "play:60",
                "tick:150:0/3",
                "tick:150:1/3",
                "tick:150:2/3",
                "step:150:49",
                "pause"
            ]
        );
        let state = state_of(&rig);
        assert_eq!(
            (state.cursor_time, state.remaining),
            (Some(from + HOUR), 49)
        );
    }

    #[test]
    fn a_timer_that_fires_late_catches_up_on_every_tick_due_so_the_bar_keeps_its_interval() {
        let (rig, from) = tick_rig();
        rig.chart.replay_set_ticks(Some(three_ticks()));
        rig.chart.replay_play(Some(60.0));
        rig.advance(60.0);
        assert_eq!(updated(&rig).len(), 1);

        // The next beat runs 70 ms late (ticks 1 AND 2 are due): the clock moved
        // with the timer gone, then the beat runs by hand.
        let timer = rig.chart.inner.state.borrow_mut().replay_timer.take();
        rig.platform.clear_timeout(timer.expect("a beat is armed"));
        rig.advance(130.0);
        rig.chart.replay_advance();
        rig.chart.replay_pause();

        assert_eq!(
            *updated(&rig).last().expect("a bar"),
            Ohlcv::new(from + HOUR, 150.0, 151.0, 149.0, 150.0, Some(1.0))
        );
        assert_eq!(
            replay_events(&rig)
                .iter()
                .filter(|e| e.starts_with("step:"))
                .count(),
            1
        );
    }

    #[test]
    fn volume_is_spread_over_ticks_without_one_and_summed_from_ticks_that_carry_one() {
        let (rig, from) = tick_rig();
        let source: ReplayTickSource = Rc::new(|bar, _| {
            let ticks = if bar.close == 150.0 {
                vec![ReplayTick::at(150.0); 4]
            } else {
                vec![
                    ReplayTick {
                        volume: Some(3.0),
                        ..ReplayTick::at(bar.close)
                    },
                    ReplayTick {
                        volume: Some(4.0),
                        ..ReplayTick::at(bar.close)
                    },
                ]
            };
            Box::pin(std::future::ready(Ok(ticks)))
        });
        rig.chart.replay_set_ticks(Some(source));

        rig.chart.replay_play(Some(20.0)); // one tick per 20 ms; the bar opens 20 ms in
        rig.advance(40.0);
        let volumes: Vec<Option<f64>> = updated(&rig).iter().map(|b| b.volume).collect();
        assert_eq!(volumes, [Some(0.25), Some(0.5)]);
        rig.advance(60.0);
        rig.chart.replay_pause();
        let next: Vec<Ohlcv> = updated(&rig)
            .into_iter()
            .filter(|b| b.time == from + 2 * HOUR)
            .collect();
        assert_eq!(next[0].volume, Some(3.0));
    }

    #[test]
    fn step_mid_bar_completes_it_on_the_stored_values() {
        let (rig, from) = tick_rig();
        rig.chart.replay_set_ticks(Some(three_ticks()));
        rig.chart.replay_play(Some(600.0));
        rig.advance(600.0); // the open is on screen; the next tick is 600 ms away
        rig.chart.replay_pause();
        assert_eq!(last_shown(&rig).close, 150.5);
        rig.log.clear();

        assert!(rig.chart.replay_step());

        assert_eq!(
            last_shown(&rig),
            Ohlcv::new(from + HOUR, 150.0, 151.0, 149.0, 150.0, Some(1.0))
        );
        assert_eq!(
            replay_events(&rig)
                .iter()
                .filter(|e| e.starts_with("step:"))
                .count(),
            1
        );
        assert!(rig.chart.replay_step(), "the next one comes whole");
        // The source is still set, so the next bar's step is a whole bar too.
        assert_eq!(last_shown(&rig).close, 151.0);
    }

    #[test]
    fn step_update_reveals_one_tick_at_a_time_opening_the_next_bar_at_its_first() {
        let (rig, from) = tick_rig();
        rig.chart.replay_set_ticks(Some(three_ticks()));

        assert!(rig.chart.replay_step_update());
        assert_eq!(
            (last_shown(&rig).time, last_shown(&rig).close),
            (from + HOUR, 150.5)
        );
        rig.chart.replay_step_update();
        assert_eq!(last_shown(&rig).close, 149.5);
        rig.chart.replay_step_update(); // the last tick settles the bar
        assert_eq!(
            last_shown(&rig),
            Ohlcv::new(from + HOUR, 150.0, 151.0, 149.0, 150.0, Some(1.0))
        );
        assert_eq!(
            replay_events(&rig)
                .iter()
                .filter(|e| e.starts_with("step:"))
                .count(),
            1
        );
        rig.chart.replay_step_update(); // the next bar opens at its first tick
        assert_eq!(
            (last_shown(&rig).time, last_shown(&rig).close),
            (from + 2 * HOUR, 151.5)
        );
    }

    #[test]
    fn step_update_without_ticks_reveals_whole_bars_and_waits_for_ticks_still_loading() {
        let (rig, from) = tick_rig();
        assert!(rig.chart.replay_step_update());
        let whole = last_shown(&rig);
        assert_eq!(
            (whole.time, whole.close, whole.high),
            (from + HOUR, 150.0, 151.0)
        );

        let release = ResolveOnce::<()>::new();
        let gate = release.clone();
        let source: ReplayTickSource = Rc::new(move |bar, _| {
            let (gate, close) = (gate.clone(), bar.close);
            Box::pin(async move {
                gate.wait().await;
                Ok(vec![ReplayTick::at(close - 0.25), ReplayTick::at(close)])
            })
        });
        rig.chart.replay_set_ticks(Some(source));
        assert!(rig.chart.replay_step_update());
        assert_eq!(last_shown(&rig).time, from + HOUR, "the ticks are loading");
        release.resolve(());
        rig.settle();
        let opened = last_shown(&rig);
        assert_eq!((opened.time, opened.close), (from + 2 * HOUR, 150.75));
    }

    #[test]
    fn an_async_source_is_asked_ahead_for_the_next_bar_and_playback_waits_for_late_ticks() {
        let (rig, from) = tick_rig();
        let asked: Rc<RefCell<Vec<(Millis, Millis)>>> = Rc::default();
        let release = ResolveOnce::<()>::new();
        let source: ReplayTickSource = {
            let (asked, release) = (Rc::clone(&asked), release.clone());
            Rc::new(move |bar, req| {
                asked.borrow_mut().push((bar.time, req.end));
                let (gate, time, close) = (release.clone(), bar.time, bar.close);
                Box::pin(async move {
                    if time == from + HOUR {
                        gate.wait().await;
                    }
                    Ok(vec![ReplayTick::at(close), ReplayTick::at(close)])
                })
            })
        };
        rig.chart.replay_set_ticks(Some(source));
        assert_eq!(
            *asked.borrow(),
            [(from + HOUR, from + 2 * HOUR)],
            "prefetched on set_ticks"
        );

        rig.chart.replay_play(Some(300.0));
        rig.advance(500.0);
        assert!(
            updated(&rig).is_empty(),
            "still waiting on the first bar's ticks"
        );

        release.resolve(());
        rig.advance(0.0);
        let first = updated(&rig)[0];
        assert_eq!((first.time, first.close), (from + HOUR, 150.0));
        let times: Vec<Millis> = asked.borrow().iter().map(|a| a.0).collect();
        assert_eq!(
            times,
            [from + HOUR, from + 2 * HOUR],
            "the next bar asked while this one plays"
        );
        rig.chart.replay_pause();
    }

    #[test]
    fn an_empty_or_failing_source_reveals_the_bar_whole() {
        let (rig, _) = tick_rig();
        let source: ReplayTickSource = Rc::new(|bar, _| {
            Box::pin(std::future::ready(if bar.close == 150.0 {
                Ok(Vec::new())
            } else {
                Err(DataError::other("no data"))
            }))
        });
        rig.chart.replay_set_ticks(Some(source));

        rig.chart.replay_play(Some(20.0));
        rig.advance(40.0);
        rig.chart.replay_pause();

        let revealed: Vec<(f64, f64)> = updated(&rig).iter().map(|b| (b.close, b.high)).collect();
        assert_eq!(revealed, [(150.0, 151.0), (151.0, 152.0)]);
        // The bar revealed whole and the one prefetched behind it both failed.
        let warnings = rig.platform.warnings();
        assert!(!warnings.is_empty());
        assert!(
            warnings
                .iter()
                .all(|w| { w.contains("replay tick source failed") && w.contains("no data") })
        );
    }

    #[test]
    fn a_pace_faster_than_a_frame_batches_ticks_so_updates_never_come_faster_than_one() {
        let (rig, _) = tick_rig();
        let ticks: Vec<ReplayTick> = (0..10)
            .map(|i| ReplayTick::at(150.0 + i as f64 / 10.0))
            .collect();
        rig.chart.replay_set_ticks(Some(Rc::new(move |_, _| {
            Box::pin(std::future::ready(Ok(ticks.clone())))
        })));

        rig.chart.replay_play(Some(5.0)); // one tick per 5 ms: 4 ticks per 20 ms update
        rig.advance(5.0 + 2.0 * 20.0);
        rig.chart.replay_pause();

        let indices: Vec<String> = replay_events(&rig)
            .into_iter()
            .filter(|e| e.starts_with("tick:"))
            .collect();
        assert_eq!(indices, ["tick:150:3/10", "tick:150:7/10", "tick:150:9/10"]);
    }

    #[test]
    fn pacing_is_pure_batching_by_the_frame_and_anchoring_on_the_ticks_applied() {
        let mut f = ReplayForming {
            bar: Ohlcv::new(0, 1.0, 1.0, 1.0, 1.0, None),
            ticks: Rc::from(vec![ReplayTick::at(1.0); 4]),
            next: 0,
            candle: None,
            volume_each: None,
            batch: 1,
            delay_ms: 0.0,
            origin: 0.0,
        };
        EngineOrchestrator::pace_forming_bar(&mut f, 5.0, 1_000.0);
        assert_eq!((f.batch, f.delay_ms, f.origin), (4, 20.0, 1_000.0));
        f.next = 3;
        EngineOrchestrator::pace_forming_bar(&mut f, 100.0, 1_000.0);
        assert_eq!((f.batch, f.delay_ms, f.origin), (1, 100.0, 800.0));
    }

    #[test]
    fn a_tick_carrying_its_span_stretches_the_candle_to_it_and_the_first_can_set_the_open() {
        let (rig, _) = tick_rig();
        let ticks = vec![
            ReplayTick {
                price: 150.2,
                open: Some(149.8),
                high: Some(150.6),
                low: Some(149.5),
                volume: None,
            },
            ReplayTick {
                high: Some(150.9),
                low: Some(149.7),
                ..ReplayTick::at(149.9)
            },
            ReplayTick::at(150.0),
        ];
        rig.chart.replay_set_ticks(Some(Rc::new(move |_, _| {
            Box::pin(std::future::ready(Ok(ticks.clone())))
        })));

        rig.chart.replay_play(Some(30.0));
        rig.advance(60.0);
        rig.chart.replay_pause();

        let u = updated(&rig);
        assert_eq!(
            (u[0].open, u[0].high, u[0].low, u[0].close),
            (149.8, 150.6, 149.5, 150.2)
        );
        assert_eq!(
            (u[1].open, u[1].high, u[1].low, u[1].close),
            (149.8, 150.9, 149.5, 149.9)
        );
    }

    #[test]
    fn unusable_ticks_are_filtered_out_of_a_sources_answer() {
        let ok = ReplayTick::at(1.0);
        let bad_price = ReplayTick::at(f64::NAN);
        let bad_open = ReplayTick {
            open: Some(f64::INFINITY),
            ..ReplayTick::at(2.0)
        };
        let bad_volume = ReplayTick {
            volume: Some(f64::NAN),
            ..ReplayTick::at(3.0)
        };
        let fine = ReplayTick {
            volume: Some(5.0),
            high: Some(9.0),
            ..ReplayTick::at(4.0)
        };
        let kept = usable_ticks(vec![ok, bad_price, bad_open, bad_volume, fine]);
        assert_eq!(&*kept, [ok, fine]);
    }

    #[test]
    fn a_seek_or_a_stop_mid_bar_puts_the_stored_bar_back_and_aborts_the_tick_requests() {
        let rig = replay_rig(200, 100, false);
        let full = shown(&rig);
        start_at(&rig, full[49].time).expect("start");
        let signals: Rc<RefCell<Vec<AbortSignal>>> = Rc::default();
        let source: ReplayTickSource = {
            let signals = Rc::clone(&signals);
            Rc::new(move |bar, req| {
                signals.borrow_mut().push(req.signal);
                let c = bar.close;
                Box::pin(std::future::ready(Ok(vec![
                    ReplayTick::at(c + 0.5),
                    ReplayTick::at(c - 0.5),
                    ReplayTick::at(c),
                ])))
            })
        };
        rig.chart.replay_set_ticks(Some(source));
        rig.chart.replay_play(Some(600.0));
        rig.advance(600.0);
        rig.chart.replay_pause();
        assert_eq!(last_shown(&rig).close, 150.5, "mid-bar");

        start_at(&rig, full[59].time).expect("forward over the forming bar");
        assert_eq!(shown(&rig)[50], full[50]);
        assert!(signals.borrow()[0].aborted());

        rig.chart.replay_play(Some(600.0));
        rig.advance(600.0);
        assert_eq!(last_shown(&rig).close, 160.5);
        rig.chart.replay_stop();
        rig.settle();
        assert_eq!(shown(&rig), full);
    }

    #[test]
    fn replacing_the_source_completes_the_forming_bar_at_once_and_none_goes_back_to_whole_bars() {
        let (rig, from) = tick_rig();
        rig.chart.replay_set_ticks(Some(three_ticks()));
        rig.chart.replay_step_update();
        assert_eq!(last_shown(&rig).close, 150.5);

        rig.chart.replay_set_ticks(None);

        assert_eq!(
            last_shown(&rig),
            Ohlcv::new(from + HOUR, 150.0, 151.0, 149.0, 150.0, Some(1.0))
        );
        assert!(rig.chart.replay_step_update());
        assert_eq!(last_shown(&rig).close, 151.0, "a whole bar");
    }

    // ── lifecycle and re-entrancy ──

    #[test]
    fn a_listener_that_stops_the_replay_from_a_step_leaves_one_end_and_the_full_history() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.log.clear();
        let weak = rig.chart.weak();
        let _sub = rig.chart.events().on(ChartEventName::ReplayStep, move |_| {
            if let Some(o) = upgrade(&weak) {
                o.replay_stop();
            }
        });

        assert!(rig.chart.replay_step());

        assert_eq!(ends(&rig), [ReplayEndReason::Stopped]);
        assert!(!state_of(&rig).active);
        assert_eq!(shown(&rig).len(), 100);
        assert_eq!(last_shown(&rig).close, 199.0);
    }

    #[test]
    fn a_play_timer_firing_after_destroy_reveals_nothing() {
        let rig = replay_rig(200, 100, false);
        start_at(&rig, shown(&rig)[49].time).expect("start");
        rig.chart.replay_play(Some(50.0));
        let revealed = rig.renderer.updated_bars.borrow().len();

        rig.chart.destroy();
        rig.advance(500.0);

        assert_eq!(rig.renderer.updated_bars.borrow().len(), revealed);
        assert!(!state_of(&rig).active);
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    #[test]
    fn a_tick_answer_landing_after_destroy_or_after_the_replay_moved_on_opens_nothing() {
        let (rig, _) = tick_rig();
        let release = ResolveOnce::<()>::new();
        let gate = release.clone();
        rig.chart.replay_set_ticks(Some(Rc::new(move |bar, _| {
            let (gate, c) = (gate.clone(), bar.close);
            Box::pin(async move {
                gate.wait().await;
                Ok(vec![ReplayTick::at(c), ReplayTick::at(c)])
            })
        })));
        rig.chart.replay_play(Some(50.0));
        rig.advance(100.0); // waiting on the ticks
        assert!(updated(&rig).is_empty());

        // A seek bumps the epoch: the old answer must not start the bar.
        let on_screen = shown(&rig);
        start_at(&rig, on_screen[10].time).expect("seek");
        release.resolve(());
        rig.settle();
        assert!(!state_of(&rig).playing);
        assert!(rig.chart.inner.state.borrow().replay_forming.is_none());

        // And after destroy the waiting task finds nothing to act on.
        rig.chart.replay_play(Some(50.0));
        rig.chart.destroy();
        rig.advance(1_000.0);
        assert!(rig.chart.inner.state.borrow().replay_forming.is_none());
    }

    #[test]
    fn a_market_switch_aborts_the_tick_requests_in_flight() {
        let (rig, _) = tick_rig();
        rig.feed.set_series("BBB", universe(200));
        let signals: Rc<RefCell<Vec<AbortSignal>>> = Rc::default();
        let source: ReplayTickSource = {
            let signals = Rc::clone(&signals);
            Rc::new(move |_, req| {
                signals.borrow_mut().push(req.signal);
                Box::pin(std::future::pending())
            })
        };
        rig.chart.replay_set_ticks(Some(source));
        assert_eq!(signals.borrow().len(), 1);
        assert!(!signals.borrow()[0].aborted());

        rig.switch(to_symbol("BBB")).expect("switch");

        assert!(signals.borrow()[0].aborted());
        assert!(
            rig.chart
                .inner
                .state
                .borrow()
                .replay_tick_fetches
                .is_empty()
        );
    }

    #[test]
    fn a_stale_tick_answer_after_an_abort_warns_nothing_when_it_fails() {
        let (rig, _) = tick_rig();
        let release = ResolveOnce::<()>::new();
        let gate = release.clone();
        rig.chart.replay_set_ticks(Some(Rc::new(move |_, _| {
            let gate = gate.clone();
            Box::pin(async move {
                gate.wait().await;
                Err(DataError::other("aborted"))
            })
        })));
        rig.chart.replay_set_ticks(None); // aborts the request
        release.resolve(());
        rig.settle();
        assert!(
            rig.platform.warnings().is_empty(),
            "an aborted request fails quietly"
        );
    }
}
