//! Bar replay across a whole workspace: every cell replays on ONE clock
//! (`WorkspaceReplay.ts`). Each chart keeps its own `chart.replay` (per-chart history,
//! indicators, watermark); this drives them all so that at any moment every chart shows
//! exactly the bars that had CLOSED by the shared replay time: a 1h chart and a 1D chart
//! next to each other never reveal the future to one another. A single-chart layout hands
//! every call straight to that chart's replay (tick replay included).
//!
//! # Differences from the original
//!
//! - The host's `onCells` subscription is three calls the host makes
//!   ([`cell_created`](WorkspaceReplay::cell_created),
//!   [`cell_destroyed`](WorkspaceReplay::cell_destroyed) and
//!   [`cell_active`](WorkspaceReplay::cell_active)); the workspace makes them from its own
//!   `cell:*` events.
//! - Events are one enum, [`WorkspaceReplayEvent`], subscribed with
//!   [`WorkspaceReplay::on`].
//! - [`WorkspaceReplay::start`] starts the other charts one after another; the original
//!   awaits them together.
//! - The pace timer runs on [`Platform::set_timeout`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use crate::controls::replay_control::{
    ReplayBounds, ReplayEndReason, ReplayStartOptions, ReplayState,
};
use crate::events::Subscription;
use crate::events::types::{ChartEvent, ChartEventName};
use crate::runtime::{LocalBoxFuture, Platform, TimerId};
use crate::vela::Vela;

/// How many bars one step may reveal on a single chart (a runaway-loop guard, never a real
/// limit).
const MAX_BARS_PER_STEP: usize = 100_000;

/// A cell as the replay reads it.
#[derive(Clone)]
pub struct ReplayCell {
    pub id: String,
    pub chart: Vela,
}

/// What the controller reads from its workspace.
pub trait WorkspaceReplayHost {
    /// Every live cell, in slot order.
    fn cells(&self) -> Vec<ReplayCell>;
    fn active_id(&self) -> Option<String>;
}

/// Options for [`WorkspaceReplay::start`].
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceReplayStartOptions {
    /// Epoch ms, read on `cell`'s chart as `chart.replay.start({ from })` reads it: that
    /// chart keeps every bar opened at or before it. The close of its last kept bar
    /// becomes the shared replay time, and every other chart keeps the bars closed by then.
    pub from: i64,
    /// The cell `from` is read on (default: the active cell).
    pub cell: Option<String>,
}

/// The workspace replay's events: the chart `replay:*` events, reported for the ACTIVE
/// cell.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceReplayEvent {
    Start {
        cursor_time: i64,
        remaining: usize,
    },
    Step {
        cursor_time: i64,
        remaining: usize,
    },
    Tick {
        cursor_time: i64,
        index: usize,
        count: usize,
    },
    Play {
        interval_ms: f64,
    },
    Pause,
    End {
        reason: ReplayEndReason,
    },
}

/// The names of [`WorkspaceReplayEvent`]s, for [`WorkspaceReplay::on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkspaceReplayEventName {
    Start,
    Step,
    Tick,
    Play,
    Pause,
    End,
}

impl WorkspaceReplayEvent {
    pub fn name(&self) -> WorkspaceReplayEventName {
        match self {
            Self::Start { .. } => WorkspaceReplayEventName::Start,
            Self::Step { .. } => WorkspaceReplayEventName::Step,
            Self::Tick { .. } => WorkspaceReplayEventName::Tick,
            Self::Play { .. } => WorkspaceReplayEventName::Play,
            Self::Pause => WorkspaceReplayEventName::Pause,
            Self::End { .. } => WorkspaceReplayEventName::End,
        }
    }
}

type Handler = Rc<dyn Fn(&WorkspaceReplayEvent)>;
type CellSubs = Vec<(Vela, Subscription<ChartEventName>)>;

struct Inner {
    host: Rc<dyn WorkspaceReplayHost>,
    platform: Rc<dyn Platform>,
    handlers: RefCell<Vec<(u64, WorkspaceReplayEventName, Handler)>>,
    next_handler: Cell<u64>,
    /// The running session: its generation and the shared replay time (every chart shows
    /// the bars closed by it).
    session: Cell<Option<(u64, f64)>>,
    generation: Cell<u64>,
    playing: Cell<bool>,
    interval_ms: Cell<f64>,
    timer: Cell<Option<TimerId>>,
    /// Cells whose replay a symbol switch ended: restarted at the shared time once their
    /// bars land.
    rejoining: RefCell<HashSet<String>>,
    cell_subs: RefCell<HashMap<String, CellSubs>>,
    /// Inside a group step: chart events are the controller's own doing.
    stepping: Cell<bool>,
    /// Inside `finish`: the charts' `replay:end` events are the controller's own doing.
    stopping: Cell<bool>,
    /// Driving a chart's own timer: its play/pause events are not the user's.
    muted: Cell<u32>,
    /// A chart finished during a group step: the session ends right after it.
    finished_during_step: Cell<bool>,
}

/// The workspace's bar-replay control surface (`workspace.replay`): the same verbs as
/// `chart.replay`, applied to every cell on a shared clock. A step moves the clock to the
/// next bar close on ANY chart, and each chart reveals the bars closed by then, so the
/// finest timeframe sets the pace and a coarser chart shows its bar when it completes.
/// Cells added while replaying join at the shared time; a cell switching symbol rejoins
/// once its new bars load; a timeframe switch keeps the cell's place. The replay ends on
/// every chart together: `stop()`, or any chart reaching its last bar. In a multi-chart
/// layout bars are revealed whole (tick replay needs a single-chart layout).
#[derive(Clone)]
pub struct WorkspaceReplay {
    inner: Rc<Inner>,
}

#[derive(Clone)]
struct WeakReplay(Weak<Inner>);

impl WeakReplay {
    fn upgrade(&self) -> Option<WorkspaceReplay> {
        self.0.upgrade().map(|inner| WorkspaceReplay { inner })
    }
}

impl WorkspaceReplay {
    pub fn new(host: Rc<dyn WorkspaceReplayHost>, platform: Rc<dyn Platform>) -> Self {
        let replay = Self {
            inner: Rc::new(Inner {
                host,
                platform,
                handlers: RefCell::default(),
                next_handler: Cell::new(0),
                session: Cell::new(None),
                generation: Cell::new(0),
                playing: Cell::new(false),
                interval_ms: Cell::new(1_000.0),
                timer: Cell::new(None),
                rejoining: RefCell::default(),
                cell_subs: RefCell::default(),
                stepping: Cell::new(false),
                stopping: Cell::new(false),
                muted: Cell::new(0),
                finished_during_step: Cell::new(false),
            }),
        };
        for cell in replay.inner.host.cells() {
            replay.watch(&cell);
        }
        replay
    }

    fn weak(&self) -> WeakReplay {
        WeakReplay(Rc::downgrade(&self.inner))
    }

    // ── public surface ──

    /// Enter replay on every cell (or seek, when already replaying), paused. Resolves once
    /// every chart shows its rewound history (a chart whose history does not reach that far
    /// loads older bars first). A chart with nothing to replay after the shared time stays
    /// as it is.
    pub fn start(&self, opts: WorkspaceReplayStartOptions) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        Box::pin(async move {
            let cells = this.inner.host.cells();
            let wanted = opts.cell.clone().or_else(|| this.inner.host.active_id());
            let Some(reference) = cells
                .iter()
                .find(|c| Some(&c.id) == wanted.as_ref())
                .or_else(|| cells.first())
                .cloned()
            else {
                return;
            };
            if this.inner.playing.get() {
                this.pause();
            }
            this.inner.rejoining.borrow_mut().clear();
            let _ = reference
                .chart
                .replay()
                .start(ReplayStartOptions { from: opts.from })
                .await;
            let Some(cursor) = reference.chart.replay().state().cursor_time else {
                return; // nothing to replay there: the chart warned
            };
            let generation = this.inner.generation.get() + 1;
            this.inner.generation.set(generation);
            let clock = bar_close(cursor as f64, reference.chart.market().timeframe.as_deref());
            this.inner.session.set(Some((generation, clock)));
            for c in this
                .inner
                .host
                .cells()
                .into_iter()
                .filter(|c| c.id != reference.id)
            {
                this.start_cell(&c, clock).await;
            }
            if this.inner.session.get().map(|s| s.0) != Some(generation) {
                return; // stopped or superseded meanwhile
            }
            this.sync_playback();
            let s = this.active_chart().map(|c| c.replay().state());
            this.emit(&WorkspaceReplayEvent::Start {
                cursor_time: s.as_ref().and_then(|s| s.cursor_time).unwrap_or(cursor),
                remaining: s.map_or(0, |s| s.remaining),
            });
        })
    }

    /// Reveal the next bar(s): the clock moves to the next bar close on any chart. Returns
    /// `false` when nothing is left.
    pub fn step(&self) -> bool {
        match self.solo() {
            Some(chart) => chart.replay().step(),
            None => self.step_group(),
        }
    }

    /// Reveal the next update: in a single-chart layout, the chart's `step_update()` (tick
    /// replay); otherwise [`step`](Self::step).
    pub fn step_update(&self) -> bool {
        match self.solo() {
            Some(chart) => chart.replay().step_update(),
            None => self.step_group(),
        }
    }

    /// Advance one update every `interval_ms` (default: the last pace, initially 1000).
    /// Calling it again while playing changes the pace.
    pub fn play(&self, interval_ms: Option<f64>) -> &Self {
        if self.inner.session.get().is_none() {
            return self;
        }
        if let Some(ms) = interval_ms {
            if !(ms.is_finite() && ms > 0.0) {
                self.inner.platform.warn(&format!(
                    "[vela] workspace.replay.play({ms}) ignored \u{2014} the interval must be a positive number of ms"
                ));
                return self;
            }
            self.inner.interval_ms.set(ms);
        }
        self.inner.playing.set(true);
        self.clear_timer();
        self.sync_playback();
        self.emit(&WorkspaceReplayEvent::Play {
            interval_ms: self.inner.interval_ms.get(),
        });
        self
    }

    pub fn pause(&self) -> &Self {
        if !self.inner.playing.get() {
            return self;
        }
        self.inner.playing.set(false);
        self.sync_playback();
        self.emit(&WorkspaceReplayEvent::Pause);
        self
    }

    /// Leave replay on every chart: full history back, live updates resumed.
    pub fn stop(&self) -> &Self {
        self.finish(ReplayEndReason::Stopped);
        self
    }

    /// `active`, `playing` and `interval_ms` for the whole workspace; the cursor fields
    /// read the ACTIVE cell's chart.
    pub fn state(&self) -> ReplayState {
        let s = if self.inner.session.get().is_some() {
            self.active_chart().map(|c| c.replay().state())
        } else {
            None
        };
        ReplayState {
            active: self.inner.session.get().is_some(),
            playing: self.inner.playing.get(),
            cursor_time: s.as_ref().and_then(|s| s.cursor_time),
            remaining: s.as_ref().map_or(0, |s| s.remaining),
            next_time: s.as_ref().and_then(|s| s.next_time),
            interval_ms: self.inner.interval_ms.get(),
        }
    }

    /// The active cell's replayable history (`chart.replay.bounds`).
    pub fn bounds(&self) -> Option<ReplayBounds> {
        self.active_chart().and_then(|c| c.replay().bounds())
    }

    /// Listen to one event. Returns the id [`off`](Self::off) takes.
    pub fn on(
        &self,
        name: WorkspaceReplayEventName,
        handler: impl Fn(&WorkspaceReplayEvent) + 'static,
    ) -> u64 {
        let id = self.inner.next_handler.get();
        self.inner.next_handler.set(id + 1);
        self.inner
            .handlers
            .borrow_mut()
            .push((id, name, Rc::new(handler)));
        id
    }

    pub fn off(&self, id: u64) {
        self.inner
            .handlers
            .borrow_mut()
            .retain(|(k, _, _)| *k != id);
    }

    pub fn destroy(&self) {
        self.inner.session.set(None);
        self.inner.playing.set(false);
        self.clear_timer();
        let subs = std::mem::take(&mut *self.inner.cell_subs.borrow_mut());
        for (_, list) in subs {
            for (chart, sub) in list {
                chart.off(&sub);
            }
        }
        self.inner.handlers.borrow_mut().clear();
    }

    // ── what the workspace tells it ──

    /// A cell was created (a layout change, a state rebuild). A cell born mid-replay joins
    /// at the shared time once its first bars are on screen.
    pub fn cell_created(&self, id: &str) {
        let Some(cell) = self.inner.host.cells().into_iter().find(|c| c.id == id) else {
            return;
        };
        self.watch(&cell);
        let Some((generation, clock)) = self.inner.session.get() else {
            return;
        };
        self.inner.rejoining.borrow_mut().insert(id.to_string());
        let this = self.clone();
        let ready = cell.chart.ready();
        let cell_id = id.to_string();
        self.inner.platform.spawn(Box::pin(async move {
            let _ = ready.await;
            let same = this.inner.session.get().map(|s| s.0) == Some(generation);
            if !same || !this.inner.rejoining.borrow_mut().remove(&cell_id) {
                return;
            }
            this.start_cell(&cell, clock).await;
            this.sync_playback();
        }));
        self.sync_playback();
    }

    /// A cell was destroyed.
    pub fn cell_destroyed(&self, id: &str) {
        self.unwatch(id);
        self.inner.rejoining.borrow_mut().remove(id);
        self.sync_playback();
    }

    /// The active cell changed (no effect of its own: the state reads the active cell live).
    pub fn cell_active(&self, _id: &str) {}

    // ── internals ──

    fn emit(&self, event: &WorkspaceReplayEvent) {
        let name = event.name();
        let handlers: Vec<Handler> = self
            .inner
            .handlers
            .borrow()
            .iter()
            .filter(|(_, n, _)| *n == name)
            .map(|(_, _, h)| h.clone())
            .collect();
        for h in handlers {
            h(event);
        }
    }

    /// The one chart of a single-chart layout: it replays on its own, tick replay included.
    fn solo(&self) -> Option<Vela> {
        let cells = self.inner.host.cells();
        (cells.len() == 1).then(|| cells[0].chart.clone())
    }

    fn active_chart(&self) -> Option<Vela> {
        let cells = self.inner.host.cells();
        let active = self.inner.host.active_id();
        cells
            .iter()
            .find(|c| Some(&c.id) == active.as_ref())
            .or_else(|| cells.first())
            .map(|c| c.chart.clone())
    }

    /// Rewind one chart to the shared time: every bar that closed by `clock`, none after.
    fn start_cell<'a>(&'a self, cell: &'a ReplayCell, clock: f64) -> LocalBoxFuture<'a, ()> {
        Box::pin(async move {
            let replay = cell.chart.replay();
            let from = last_open_closed_by(clock, cell.chart.market().timeframe.as_deref());
            let Some(bounds) = replay.bounds() else {
                return;
            };
            if bounds.last as f64 <= from {
                return; // no bar after the shared time (a market paused, or not loaded)
            }
            let _ = replay.start(ReplayStartOptions { from: from as i64 }).await;
        })
    }

    /// Move the clock to the next bar close on any chart; each chart reveals what closed by
    /// then.
    fn step_group(&self) -> bool {
        let Some((generation, _)) = self.inner.session.get() else {
            return false;
        };
        let cells = self.inner.host.cells();
        let mut next = f64::INFINITY;
        for c in &cells {
            if let Some(t) = c.chart.replay().state().next_time {
                let close = bar_close(t as f64, c.chart.market().timeframe.as_deref());
                next = crate::indicators::classics::math::js_min(next, close);
            }
        }
        if !next.is_finite() {
            return false;
        }
        self.inner.stepping.set(true);
        for c in &cells {
            let tf = c.chart.market().timeframe;
            for _ in 0..MAX_BARS_PER_STEP {
                let Some(t) = c.chart.replay().state().next_time else {
                    break;
                };
                if bar_close(t as f64, tf.as_deref()) > next {
                    break;
                }
                if !c.chart.replay().step() {
                    break;
                }
            }
        }
        self.inner.stepping.set(false);
        self.inner.session.set(Some((generation, next)));
        if self.inner.finished_during_step.replace(false) {
            self.finish(ReplayEndReason::Finished);
            return true;
        }
        if let Some(s) = self.active_chart().map(|c| c.replay().state())
            && let Some(cursor) = s.cursor_time
        {
            self.emit(&WorkspaceReplayEvent::Step {
                cursor_time: cursor,
                remaining: s.remaining,
            });
        }
        true
    }

    /// Put the timers in the state `playing` asks for: a single chart runs its own (it
    /// paces ticks), several run on the controller's clock.
    fn sync_playback(&self) {
        let solo = self.solo();
        let run = self.inner.playing.get() && self.inner.session.get().is_some();
        self.inner.muted.set(self.inner.muted.get() + 1);
        for c in self.inner.host.cells() {
            let state = c.chart.replay().state();
            let is_solo = solo.is_some(); // a single-chart layout: this is its only cell
            if is_solo && run {
                if !state.playing || state.interval_ms != self.inner.interval_ms.get() {
                    c.chart.replay().play(Some(self.inner.interval_ms.get()));
                }
            } else if state.playing {
                c.chart.replay().pause();
            }
        }
        self.inner
            .muted
            .set(self.inner.muted.get().saturating_sub(1));
        if !run || solo.is_some() {
            self.clear_timer();
        } else if self.inner.timer.get().is_none() {
            self.schedule();
        }
    }

    fn schedule(&self) {
        let weak = self.weak();
        let id = self.inner.platform.set_timeout(
            self.inner.interval_ms.get(),
            Box::new(move || {
                let Some(this) = weak.upgrade() else { return };
                this.inner.timer.set(None);
                if !this.inner.playing.get()
                    || this.inner.session.get().is_none()
                    || this.solo().is_some()
                {
                    return;
                }
                if this.step_group() || !this.inner.rejoining.borrow().is_empty() {
                    if this.inner.playing.get()
                        && this.inner.session.get().is_some()
                        && this.inner.timer.get().is_none()
                    {
                        this.schedule();
                    }
                } else {
                    this.pause(); // nothing left to step on any chart
                }
            }),
        );
        self.inner.timer.set(Some(id));
    }

    fn clear_timer(&self) {
        if let Some(id) = self.inner.timer.take() {
            self.inner.platform.clear_timeout(id);
        }
    }

    /// End the session on every chart (idempotent).
    fn finish(&self, reason: ReplayEndReason) {
        let had = self.inner.session.get().is_some();
        self.inner.session.set(None);
        self.inner.playing.set(false);
        self.clear_timer();
        self.inner.rejoining.borrow_mut().clear();
        self.inner.stopping.set(true);
        for c in self.inner.host.cells() {
            c.chart.replay().stop(); // also cancels a start still loading history
        }
        self.inner.stopping.set(false);
        if had {
            self.emit(&WorkspaceReplayEvent::End { reason });
        }
    }

    fn watch(&self, cell: &ReplayCell) {
        self.unwatch(&cell.id);
        let chart = &cell.chart;
        let id = cell.id.clone();
        let mut subs = Vec::new();
        let weak = self.weak();
        let w = weak.clone();
        let cid = id.clone();
        subs.push((
            chart.clone(),
            chart.on(ChartEventName::ReplayEnd, move |ev| {
                let (Some(this), ChartEvent::ReplayEnd { reason }) = (w.upgrade(), ev) else {
                    return;
                };
                if this.inner.session.get().is_none() || this.inner.stopping.get() {
                    return;
                }
                if *reason == ReplayEndReason::Market {
                    this.inner.rejoining.borrow_mut().insert(cid.clone());
                } else if this.inner.stepping.get() {
                    if *reason == ReplayEndReason::Finished {
                        this.inner.finished_during_step.set(true);
                    }
                } else {
                    this.finish(*reason);
                }
            }),
        ));
        let w = weak.clone();
        let cid = id.clone();
        subs.push((
            chart.clone(),
            chart.on(ChartEventName::MarketChanged, move |_| {
                let Some(this) = w.upgrade() else { return };
                if !this.inner.rejoining.borrow_mut().remove(&cid) {
                    return;
                }
                let Some((_, clock)) = this.inner.session.get() else {
                    return;
                };
                let Some(cell) = this.inner.host.cells().into_iter().find(|c| c.id == cid) else {
                    return;
                };
                let t = this.clone();
                this.inner.platform.spawn(Box::pin(async move {
                    t.start_cell(&cell, clock).await;
                    t.sync_playback();
                }));
            }),
        ));
        let w = weak.clone();
        let cid = id.clone();
        subs.push((
            chart.clone(),
            chart.on(ChartEventName::ReplayStep, move |ev| {
                let (
                    Some(this),
                    ChartEvent::ReplayStep {
                        cursor_time,
                        remaining,
                    },
                ) = (w.upgrade(), ev)
                else {
                    return;
                };
                if this.inner.session.get().is_some()
                    && !this.inner.stepping.get()
                    && this.is_active(&cid)
                {
                    this.emit(&WorkspaceReplayEvent::Step {
                        cursor_time: *cursor_time,
                        remaining: *remaining,
                    });
                }
            }),
        ));
        let w = weak.clone();
        let cid = id.clone();
        subs.push((
            chart.clone(),
            chart.on(ChartEventName::ReplayTick, move |ev| {
                let (
                    Some(this),
                    ChartEvent::ReplayTick {
                        cursor_time,
                        index,
                        count,
                    },
                ) = (w.upgrade(), ev)
                else {
                    return;
                };
                if this.inner.session.get().is_some() && this.is_active(&cid) {
                    this.emit(&WorkspaceReplayEvent::Tick {
                        cursor_time: *cursor_time,
                        index: *index,
                        count: *count,
                    });
                }
            }),
        ));
        // A single chart runs its own timer: a pause it takes on its own (a seek) is the
        // session's.
        let w = weak;
        let cid = id.clone();
        subs.push((
            chart.clone(),
            chart.on(ChartEventName::ReplayPause, move |_| {
                let Some(this) = w.upgrade() else { return };
                if this.inner.session.get().is_none()
                    || this.inner.muted.get() > 0
                    || !this.inner.playing.get()
                {
                    return;
                }
                let is_solo = this.inner.host.cells().len() == 1
                    && this.inner.host.cells().first().is_some_and(|c| c.id == cid);
                if !is_solo {
                    return;
                }
                this.inner.playing.set(false);
                this.emit(&WorkspaceReplayEvent::Pause);
            }),
        ));
        self.inner.cell_subs.borrow_mut().insert(id, subs);
    }

    fn unwatch(&self, id: &str) {
        let subs = self.inner.cell_subs.borrow_mut().remove(id);
        for (chart, sub) in subs.into_iter().flatten() {
            chart.off(&sub);
        }
    }

    fn is_active(&self, id: &str) -> bool {
        let active = self
            .inner
            .host
            .active_id()
            .or_else(|| self.inner.host.cells().first().map(|c| c.id.clone()));
        active.as_deref() == Some(id)
    }
}

// ── bar close arithmetic ───────────────────────────────────────

/// How many calendar months a month-based timeframe spans (`M`, `3M`, ...); 0 otherwise.
fn month_span(timeframe: Option<&str>) -> i64 {
    let t = timeframe.unwrap_or("").trim();
    let Some(digits) = t.strip_suffix('M') else {
        return 0;
    };
    if digits.is_empty() {
        return 1;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    // `Number(m[1] || 1)`: "0" is a non-empty string, so it reads as 0 months.
    digits.parse::<i64>().unwrap_or(0)
}

/// Days from 1970-01-01 of a proleptic Gregorian civil date (`month` is 1-based).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count from 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `Date.UTC(y, month0 + months, d, h, min)`: months beyond the year roll over, and a day
/// past the end of the month rolls into the next one, as the JS date does.
fn utc_shifted(ms: f64, months: i64) -> f64 {
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let total = y * 12 + (m - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let minutes = rem / 60_000; // seconds and milliseconds drop, as Date.UTC reads only h and min
    (days_from_civil(ny, nm, 1) + (d - 1)) as f64 * 86_400_000.0 + (minutes * 60_000) as f64
}

/// When a bar opened at `open` closes on `timeframe`: calendar months for month-based
/// ones.
pub fn bar_close(open: f64, timeframe: Option<&str>) -> f64 {
    let months = month_span(timeframe);
    if months > 0 {
        return utc_shifted(open, months);
    }
    open + crate::data::timeframe_to_ms(timeframe.unwrap_or("60")) as f64
}

/// The latest open whose bar has closed by `time` on `timeframe` (the inverse of
/// [`bar_close`]).
pub fn last_open_closed_by(time: f64, timeframe: Option<&str>) -> f64 {
    let months = month_span(timeframe);
    if months > 0 {
        return utc_shifted(time, -months);
    }
    time - crate::data::timeframe_to_ms(timeframe.unwrap_or("60")) as f64
}

#[cfg(test)]
mod tests;
