//! The market section of the orchestrator: construction-time wiring (`init`),
//! generations and supersede waiters, the load pipeline (offline data, preview
//! then deep load, single load, progressive load, chunked backfill), `set_market`,
//! live bars and gap healing, price format and symbol facts, the price style
//! (bar transform and chart-type data engines), viewport, theme, visible range
//! and `destroy`.
//!
//! Placement decisions: `blank_indicator_visuals` and `reexecute_indicators` live
//! here, not in the indicators section. They are what a wholesale change of the
//! bars does to the mounted indicators (a market switch, a price-style change, a
//! replay cut); they only walk the records and call `set_loading` and
//! `execute_indicator` of the indicators section.

use serde_json::json;

use super::replay::EndReplayOpts;
use super::*;
use crate::chart_types::SeriesDataEngineHost;
use crate::data::provider_registry::parse_symbol;
use crate::data_unavailable::DataUnavailableError;
use crate::engine::inspect::{SceneInspection, inspect_models};
use crate::events::types::{HistoryReason, MarketIdentity};
use crate::i18n::text_of;
use crate::model::{IndicatorModel, SeriesData};
use crate::options::{MarketSnapshot, MarketSwitch};
use crate::ports::chart_renderer::{SetBarsOptions, VisibleRange, VisibleRangeOptions};
use crate::ports::market_data::{BarRange, BatchSink, InitialRange};
use crate::ports::scripting_engine::{BarsChangeReason, ExecutionMarket};
use crate::price_styles::parse_extended_ticker;
use crate::visible_range::{VisibleRangePreset, preset_to_range};

/// What a series load may be asked.
#[derive(Clone, Copy)]
struct LoadOpts {
    /// Activates the bar-following layers once (volume auto-add): a market SWITCH
    /// must not re-add what the user has since removed.
    first_load: bool,
}

/// How `set_bar_series` hands the series to the renderer.
#[derive(Clone, Copy, Default)]
pub(super) struct SetBarOpts {
    pub preserve_view: bool,
    /// The switch-time clearing call: an empty series that must not end the load.
    pub clearing: bool,
}

/// Who settled the `set_market` race: the load pipeline, or a supersession.
#[derive(Clone)]
enum LoadRace {
    Loaded(Result<(), DataError>),
    Superseded,
}

/// What `set_market` decided before its load and the code after it needs.
struct Switch {
    identity_changed: bool,
    depth_only: bool,
    carried: Option<VisibleBarRange>,
    carried_count: usize,
    replay_carry: Option<Rc<ReplayCarry>>,
    prev: MarketIdentity,
}

fn ready<T: 'static>(value: T) -> LocalBoxFuture<'static, T> {
    Box::pin(std::future::ready(value))
}

impl EngineOrchestrator {
    /// The visible range to feed a run. Prefer the latest viewport-change event,
    /// but fall back to querying the renderer directly: the native renderer
    /// doesn't emit a viewport-change for its initial fit, so on the first run
    /// the event hasn't fired yet and a visible-range script (e.g.
    /// `chart.left_visible_bar_time`) would otherwise see the whole dataset
    /// instead of the visible window.
    pub(super) fn current_visible_range(&self) -> Option<VisibleBarRange> {
        if let Some(range) = self.inner.state.borrow().visible_range {
            return Some(range);
        }
        self.inner
            .renderer
            .get_visible_range()
            .map(|r| VisibleBarRange {
                left: r.from as f64,
                right: r.to as f64,
            })
    }

    // Used by the indicators section once ported.
    #[allow(dead_code)]
    /// Resolve the engine for a language, defaulting when unspecified.
    pub(super) fn engine_for(
        &self,
        language: Option<&str>,
    ) -> Result<Rc<dyn ScriptingEngine>, EngineError> {
        let language = language.unwrap_or(&self.inner.default_language);
        match self.inner.state.borrow().engines.get(language) {
            Some(engine) => Ok(Rc::clone(engine)),
            None => Err(EngineError::new(format!(
                "[vela] no scripting engine registered for language \"{language}\". Vela ships none \u{2014} \
                 install one (Pine Script: @luxalgo/vela-pinets) and register it before addIndicator, \
                 e.g. chart.registerEngine('pine', new PineEngine())."
            ))),
        }
    }

    // Used by the indicators section once ported.
    #[allow(dead_code)]
    /// Market metadata for an execution (Vela owns the bars).
    pub(super) fn market(&self) -> ExecutionMarket {
        let (cfg, style) = {
            let st = self.inner.state.borrow();
            (st.config.market.clone(), st.price_style.clone())
        };
        ExecutionMarket {
            symbol: cfg.symbol.clone().unwrap_or_else(|| "TEST".to_string()),
            timeframe: cfg.timeframe.clone().unwrap_or_else(|| "60".to_string()),
            symbol_info: self.inner.feed.symbol_info(&cfg),
            chart_style: Some(style),
            session: cfg.session.map(|s| s.as_str().to_string()),
        }
    }

    /// The data gateway handed to engines: fetch ANY `(symbol, timeframe)` series
    /// through the feed + cache. Used for secondary series (`request.security`
    /// HTF/LTF/cross-symbol). The symbol is passed through so the feed resolves
    /// its OWN `provider:` prefix (cross-exchange), falling back to the chart's
    /// provider for a bare symbol. No aggregation; timeframes stay separate.
    ///
    /// Chart-type modifiers: providers only ever serve RAW market data; any
    /// transform is applied here, above them, on the fetched series' OWN
    /// timeframe. The EXTENDED ticker decides, explicitly: `"SYM;heikinashi"`
    /// fetches the derived series, anything else (including `"SYM;standard"`)
    /// fetches raw. The engine composes the modifier: on a Heikin Ashi chart
    /// `syminfo.tickerid` carries `";heikinashi"`, so default same-symbol
    /// `request.security` calls inherit the chart type, while a plain
    /// `ticker.standard()` symbol explicitly opts back into standard data.
    pub(super) fn fetch_series(
        &self,
        symbol: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        // The feed call is the synchronous prefix of the original `async` method.
        let parsed = parse_extended_ticker(&self.inner.chart_types.borrow(), symbol);
        let cfg = MarketConfig {
            symbol: Some(parsed.symbol.clone()),
            timeframe: Some(timeframe.to_string()),
            ..MarketConfig::default()
        };
        let ranged = self.inner.feed.load_range(&cfg, range);
        let pending = match ranged {
            Some(pending) => pending,
            None => {
                let cfg = MarketConfig {
                    bars: range.limit,
                    ..cfg
                };
                self.inner.feed.load(&cfg)
            }
        };
        let transform = parsed.transform;
        Box::pin(async move {
            let bars = js_await(pending).await?;
            Ok(match transform {
                Some(t) => t.full(&bars),
                None => bars,
            })
        })
    }

    pub(super) fn on_viewport_change(&self, range: VisibleRange) {
        let previous = {
            let mut st = self.inner.state.borrow_mut();
            st.visible_range = Some(VisibleBarRange {
                left: range.from as f64,
                right: range.to as f64,
            });
            st.viewport_timer.take()
        };
        self.emit(ChartEvent::ViewportChanged {
            from: range.from,
            to: range.to,
        });
        if let Some(timer) = previous {
            self.inner.platform.clear_timeout(timer);
        }
        let weak = self.weak();
        let timer = self.inner.platform.set_timeout(
            VIEWPORT_DEBOUNCE_MS,
            Box::new(move || {
                let Some(o) = upgrade(&weak) else { return };
                o.run_viewport_debounce();
            }),
        );
        self.inner.state.borrow_mut().viewport_timer = Some(timer);
    }

    /// The debounced half of a viewport change: re-run ONLY the consumers that
    /// depend on the window.
    fn run_viewport_debounce(&self) {
        let window = {
            let mut st = self.inner.state.borrow_mut();
            st.viewport_timer = None;
            if st.destroyed || st.switching_market {
                // Old sessions must not re-run mid-switch.
                return;
            }
            let Some(window) = st.visible_range else {
                return;
            };
            window
        };
        for record in self.inner.registry.all() {
            // What to poke is decided under one short borrow; the call goes out after.
            enum Poke {
                None,
                Session(Rc<dyn crate::ports::scripting_engine::ExecutionSession>),
                Native,
            }
            let poke = {
                let mut r = record.borrow_mut();
                if r.render_handle.is_none() || r.hidden {
                    Poke::None
                } else if let Some(session) = r
                    .session
                    .clone()
                    .filter(|_| r.prepared.as_ref().is_some_and(|p| p.reacts_to_viewport()))
                {
                    // Range-aware indicators re-run / refetch with the new window.
                    r.pending_cause = Some(ScriptRunCause::Viewport);
                    Poke::Session(session)
                } else if r
                    .native
                    .as_ref()
                    .is_some_and(|n| n.descriptor.reacts_to_viewport)
                {
                    Poke::Native
                } else {
                    Poke::None
                }
            };
            match poke {
                Poke::None => {}
                Poke::Session(session) => session.set_visible_range(window),
                Poke::Native => self.native_on_viewport(
                    &record,
                    VisibleRange {
                        from: window.left as Millis,
                        to: window.right as Millis,
                    },
                ),
            }
        }
        // An active chart-type data engine streams in newly-visible data as the
        // user scrolls.
        if let Some(engine) = self.active_type_engine() {
            engine.borrow_mut().on_viewport(VisibleRange {
                from: window.left as Millis,
                to: window.right as Millis,
            });
        }
    }

    /// The active style's data engine, if one is running.
    fn active_type_engine(&self) -> Option<TypeEngine> {
        let st = self.inner.state.borrow();
        st.active_engine_style
            .as_ref()
            .and_then(|style| st.type_engines.get(style).cloned())
    }

    /// Cancel the debounce timer, if one is armed.
    fn clear_viewport_timer(&self) {
        let timer = self.inner.state.borrow_mut().viewport_timer.take();
        if let Some(timer) = timer {
            self.inner.platform.clear_timeout(timer);
        }
    }

    /// The first load: bump the generation, enter the loading state, run the
    /// load pipeline, then go live and announce `ready`. The synchronous prefix
    /// (generation, `load:start`, the facts probe, the first fetch call) runs
    /// inside the call, as the original's constructor does.
    pub(super) fn init(&self) {
        let epoch = self.bump_generation();
        // First load: nothing painted until the first batch.
        self.begin_load(true);
        // The first call into the pipeline is the synchronous prefix.
        let load = eager(self.load_market(epoch, LoadOpts { first_load: true }));
        let this = self.clone();
        self.spawn(Box::pin(async move {
            // `await this.loadMarket(...)`.
            let result = js_await(load).await;
            let outcome = match result {
                Err(e) => Err(e),
                Ok(()) => {
                    // Superseded mid-load (a set_market, or destroy): neither
                    // goes live nor announces.
                    if this.generation() == epoch {
                        this.start_live();
                        this.emit(ChartEvent::Ready);
                    }
                    Ok(())
                }
            };
            this.inner.ready.resolve(outcome);
        }));
    }

    /// Resolves once the first load pipeline has ended (`Err` when it failed).
    pub fn ready(&self) -> LocalBoxFuture<'static, Result<(), DataError>> {
        let latch = self.inner.ready.clone();
        Box::pin(async move { latch.wait().await })
    }

    /// Bump the generation, superseding every detached continuation, and release
    /// superseded `set_market` awaiters so their futures resolve instead of
    /// hanging.
    pub(super) fn bump_generation(&self) -> u64 {
        let (epoch, abort, waiters) = {
            let mut st = self.inner.state.borrow_mut();
            st.generation += 1;
            (
                st.generation,
                st.progressive_abort.take(),
                std::mem::take(&mut st.supersede_waiters),
            )
        };
        // The superseded load's source stops polling promptly.
        if let Some(abort) = abort {
            abort.abort();
        }
        for waiter in waiters {
            waiter();
        }
        epoch
    }

    /// The source refused THIS chart's market for good. Only the chart's own
    /// pipeline lands here (the initial/switch load, the history backfill and the
    /// gap heal, each tagged with its load generation); the engines'
    /// `fetch_series` gateway never does (its future errors to the script
    /// instead). Ends the load like an empty answer would, stops live updates so
    /// nothing keeps polling a series that will never come, and reports once per
    /// load. The timeframe stays as set: falling back is the host's policy, not
    /// the core's.
    pub(super) fn market_unavailable(&self, epoch: u64, e: &DataUnavailableError) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.generation != epoch || st.unavailable_gen == Some(epoch) {
                return;
            }
            st.unavailable_gen = Some(epoch);
        }
        self.warn(&format!("[vela] {}", e.message()));
        self.stop_live();
        self.complete_history(HistoryReason::Aborted);
        self.end_load();
        let (symbol, timeframe) = self.market_identity_or_defaults();
        self.emit(ChartEvent::DataUnavailable {
            symbol,
            timeframe,
            reason: e.reason.clone(),
            finest: e.finest.clone(),
            default: e.default.clone(),
        });
    }

    /// The market's symbol and timeframe with the original's defaults.
    fn market_identity_or_defaults(&self) -> (String, String) {
        let st = self.inner.state.borrow();
        let m = &st.config.market;
        (
            m.symbol.clone().unwrap_or_else(|| "TEST".to_string()),
            m.timeframe.clone().unwrap_or_else(|| "60".to_string()),
        )
    }

    /// Enter the loading state: raise the renderer's affordance and tell the
    /// plugins (`load:start`) BEFORE the series is blanked or fetched, so
    /// extensions and custom indicators can hide or reset their own visuals
    /// first. Transition-guarded: a switch superseding a still-loading switch
    /// re-enters silently.
    pub(super) fn begin_load(&self, first_load: bool) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.loading_up {
                return;
            }
            st.loading_up = true;
        }
        self.inner.renderer.set_loading(true);
        let (symbol, timeframe) = self.market_identity_or_defaults();
        self.emit(ChartEvent::LoadStart {
            symbol,
            timeframe,
            first_load,
        });
    }

    /// Leave the loading state (first bars painted, or a load that ended with
    /// none): drops the affordance and fires the matching `load:end`.
    pub(super) fn end_load(&self) {
        let bars = {
            let mut st = self.inner.state.borrow_mut();
            if !st.loading_up {
                return;
            }
            st.loading_up = false;
            st.bars.len()
        };
        self.inner.renderer.set_loading(false);
        let (symbol, timeframe) = self.market_identity_or_defaults();
        self.emit(ChartEvent::LoadEnd {
            symbol,
            timeframe,
            bars,
        });
    }

    /// Load the configured market's history into the chart: THE shared load
    /// pipeline behind `init` and `set_market`. Up to `SINGLE_LOAD_BARS` the whole
    /// depth comes in ONE request: a round trip dominates the row count, and
    /// splitting the range buys a faster first candle that nothing can use: an
    /// indicator's first run is held until the full depth lands anyway. DEEPER
    /// than that, the load is progressive: the newest `CHUNK_BARS` paint
    /// immediately and the rest streams in behind them through the shared
    /// backfill loop, so the pipeline resolves at the first paint (await
    /// `history_complete` for the full depth) and sessions started meanwhile are
    /// stamped backfill until complete. Offline data is a single fetch; a
    /// rangeless feed keeps the preview-then-full shape, since it cannot fetch a
    /// range at all. EVERY await is followed by a generation check so a
    /// superseded load (newer `set_market`, destroy) abandons without touching
    /// the chart.
    fn load_market(
        &self,
        epoch: u64,
        opts: LoadOpts,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        let this = self.clone();
        Box::pin(async move {
            let result = js_await(this.load_market_inner(epoch, opts)).await;
            let result = match result {
                Err(DataError::Unavailable(e)) => {
                    this.market_unavailable(epoch, &e);
                    Ok(())
                }
                other => other,
            };
            // The owning load always ends the loading state: this is the belt for
            // the paths that never hand the renderer a non-empty series (a failed
            // fetch, an empty market). A superseded load leaves the state to its
            // successor.
            if this.generation() == epoch {
                this.end_load();
            }
            result
        })
    }

    fn load_market_inner(
        &self,
        epoch: u64,
        opts: LoadOpts,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        let this = self.clone();
        Box::pin(async move {
            let market = this.market_cfg();
            // The new market's facts decide the style its first candles draw in:
            // known ones apply now, the rest as soon as they land (the load's own
            // metadata request, usually ahead of the bars). A symbol switch starts
            // from unknown, so the chosen style comes back.
            this.sync_symbol_facts();
            let requested = market.bars.unwrap_or(500);
            let initial_range = market.visible_range;
            let has_data = market.data.as_ref().is_some_and(|d| !d.is_empty());
            // Only a DEEP request is worth splitting, and only when a requested
            // initial window does not pin the frame (a chunked load would paint the
            // wrong range, then jump).
            let deep = !has_data && initial_range.is_none() && requested > SINGLE_LOAD_BARS;
            // PROGRESSIVE-CAPABLE source first: paint every snapshot it emits while
            // it heals (first candles in seconds on a cold symbol, depth growing
            // behind them) and complete when the final answer resolves. The
            // pipeline returns at the FIRST paint (exactly the deep head + backfill
            // shape below); the remainder streams in, generation-checked. Snapshots
            // are cumulative and confirmed-from-the-newest-bar by the port contract,
            // so each paint replaces the series with a superset, viewport preserved.
            // A feed without the capability (the method answers `None`, or the
            // resolved value does) leaves the classic paths below untouched.
            let mut progressive_served = false;
            if !has_data && initial_range.is_none() && this.inner.feed.supports_load_progressive() {
                progressive_served = this
                    .load_progressive_path(epoch, market.clone(), requested, opts.first_load)
                    .await;
            }
            if progressive_served {
                // Painted above; the final snapshot and completion stream in behind.
            } else if deep && this.inner.feed.supports_load_range() {
                // Paint the newest chunk, stream the rest in behind it. The first
                // chunk is a full CHUNK_BARS, not a token head: at this depth the
                // round trips are what cost, so each one carries as much as the
                // source answers well.
                let head_cfg = MarketConfig {
                    bars: Some(requested.min(CHUNK_BARS)),
                    ..market.clone()
                };
                let head = js_await(this.inner.feed.load(&head_cfg)).await?;
                if this.generation() != epoch {
                    return Ok(());
                }
                let head_len = head.len();
                this.set_bar_series(head, SetBarOpts::default());
                if opts.first_load {
                    // Bar-following layers ride the FIRST candles.
                    this.activate_bar_layers();
                }
                if head_len > 0 && requested > head_len {
                    this.inner.state.borrow_mut().history_state = HistoryState::Backfill;
                    this.detach(this.backfill_history(epoch, requested));
                } else {
                    // The source had no more than one chunk of history.
                    this.complete_history(HistoryReason::Depth);
                }
            } else if deep {
                // RANGELESS feed at depth: it cannot fetch a range, so chunking
                // would re-download the head every time: keep the two-fetch shape,
                // a quick preview then everything.
                let preview_cfg = MarketConfig {
                    bars: Some(PREVIEW_BARS),
                    ..market.clone()
                };
                let preview = js_await(this.inner.feed.load(&preview_cfg)).await?;
                if this.generation() != epoch {
                    return Ok(());
                }
                this.set_bar_series(preview, SetBarOpts::default());
                if opts.first_load {
                    // Bar-following layers follow the FIRST candles, not the deep fetch.
                    this.activate_bar_layers();
                }
                let full = js_await(this.inner.feed.load(&market)).await?;
                if this.generation() != epoch {
                    return Ok(());
                }
                this.set_bar_series(
                    full,
                    SetBarOpts {
                        preserve_view: true,
                        ..SetBarOpts::default()
                    },
                );
                this.complete_history(HistoryReason::Depth);
            } else {
                let bars = js_await(this.inner.feed.load(&market)).await?;
                if this.generation() != epoch {
                    return Ok(());
                }
                this.set_bar_series(bars, SetBarOpts::default());
                if opts.first_load {
                    this.activate_bar_layers();
                }
                this.complete_history(HistoryReason::Depth);
            }
            // Frame the requested window NOW: `set_bar_series` only invalidated, so
            // this lands in the very first painted frame: no wrong-window flash, no
            // re-frame jump.
            match initial_range {
                Some(InitialRange::Preset(preset)) => this.set_visible_range_preset(preset),
                Some(InitialRange::Range(range)) => this
                    .inner
                    .renderer
                    .set_visible_range(range, VisibleRangeOptions::default()),
                None => {}
            }
            // Price-axis precision: try the (already cached) tick size now, then
            // resolve async so the axis switches from the zoom formula to the
            // symbol's true precision as soon as it lands.
            this.refresh_price_format();
            this.detach(this.ensure_price_format());
            // Backstop for facts the early probe could not reach (the symbol was
            // not resolvable before the provider indexes settled; a custom feed
            // filling its cache with the bars).
            if this.inner.state.borrow().facts.is_none() {
                this.sync_symbol_facts();
            }
            Ok(())
        })
    }

    /// The progressive-source path of the load: returns whether the source served
    /// the load (the first paint happened, or supersession released the wait). The
    /// completion runs detached behind it.
    fn load_progressive_path(
        &self,
        epoch: u64,
        market: MarketConfig,
        requested: usize,
        first_load: bool,
    ) -> LocalBoxFuture<'static, bool> {
        let this = self.clone();
        Box::pin(async move {
            let painted = Rc::new(Cell::new(false));
            let weak = this.weak();
            let paint: Rc<dyn Fn(Vec<Ohlcv>, bool)> = {
                let painted = Rc::clone(&painted);
                Rc::new(move |bars: Vec<Ohlcv>, is_final: bool| {
                    let Some(o) = upgrade(&weak) else { return };
                    if o.generation() != epoch || (!is_final && bars.is_empty()) {
                        return;
                    }
                    // Hold the framing paint until it can carry the view.
                    if !painted.get() && !is_final && bars.len() < requested.min(FIRST_PAINT_BARS) {
                        return;
                    }
                    if painted.get() && o.inner.state.borrow().replay_queue.is_some() {
                        // A replay started before the history converged keeps its cut.
                        o.merge_history_into_replay(&bars);
                        return;
                    }
                    let repaint = painted.get();
                    let any = !bars.is_empty();
                    o.set_bar_series(
                        bars,
                        SetBarOpts {
                            preserve_view: repaint,
                            ..SetBarOpts::default()
                        },
                    );
                    if !painted.get() && any {
                        painted.set(true);
                        if first_load {
                            o.activate_bar_layers();
                        }
                        if !is_final {
                            o.inner.state.borrow_mut().history_state = HistoryState::Backfill;
                        }
                    }
                    // A deeper snapshot is a prepend like any backfill chunk: the
                    // consumers that follow the history as it grows re-run over it.
                    // The final one is followed by the completion, which pokes
                    // everyone anyway.
                    if repaint && !is_final {
                        o.notify_sessions_bars(Some(BarsChangeReason::Backfill));
                    }
                })
            };
            let abort = AbortController::new();
            this.inner.state.borrow_mut().progressive_abort = Some(abort.clone());
            // Settled once: the first paint (or the source's answer that it cannot
            // serve), whichever comes first.
            let first_paint = ResolveOnce::<bool>::new();
            // Supersession must release THIS wait immediately (the newer switch
            // owns the chart): the provider's own resolution can lag its abort by
            // one poll.
            {
                let first_paint = first_paint.clone();
                abort.signal().on_abort(move || {
                    first_paint.resolve(true);
                });
            }
            let sink: BatchSink = {
                let (paint, painted, first_paint) =
                    (Rc::clone(&paint), Rc::clone(&painted), first_paint.clone());
                Box::new(move |bars| {
                    paint(bars, false);
                    if painted.get() {
                        first_paint.resolve(true);
                    }
                })
            };
            let Some(pending) =
                this.inner
                    .feed
                    .load_progressive(&market, sink, Some(abort.signal()))
            else {
                // Incapable: the classic paths take over.
                this.clear_progressive_abort(epoch);
                return false;
            };
            // The source's own resolution, detached: it keeps running after the
            // pipeline returned at the first paint.
            {
                let (o, paint, first_paint) =
                    (this.clone(), Rc::clone(&paint), first_paint.clone());
                this.spawn(Box::pin(async move {
                    match pending.await {
                        Ok(full) => {
                            o.clear_progressive_abort(epoch);
                            match full {
                                // Incapable: the classic paths take over.
                                None => {
                                    first_paint.resolve(false);
                                }
                                Some(full) => {
                                    if o.generation() != epoch {
                                        first_paint.resolve(true);
                                        return;
                                    }
                                    let len = full.len();
                                    paint(full, true);
                                    o.complete_history(if len >= requested {
                                        HistoryReason::Depth
                                    } else {
                                        HistoryReason::Genesis
                                    });
                                    first_paint.resolve(true);
                                }
                            }
                        }
                        Err(e) => {
                            o.clear_progressive_abort(epoch);
                            match e {
                                DataError::Unavailable(unavailable) => {
                                    o.market_unavailable(epoch, &unavailable);
                                }
                                DataError::Other(_) => {
                                    if o.generation() == epoch {
                                        o.complete_history(HistoryReason::Aborted);
                                    }
                                }
                            }
                            first_paint.resolve(true);
                        }
                    }
                }));
            }
            // `await new Promise(...)`: resumes a turn after the first paint.
            js_await(first_paint.wait()).await
        })
    }

    /// Forget the progressive abort handle of load `epoch`, unless a newer load
    /// already installed its own (the original compares the controller; a
    /// generation owns at most one).
    fn clear_progressive_abort(&self, epoch: u64) {
        let mut st = self.inner.state.borrow_mut();
        if st.generation == epoch {
            st.progressive_abort = None;
        }
    }

    /// Start the live tick subscription + arm the tab-restore catch-up: never for
    /// a market the source refused (nothing would ever stream).
    pub(super) fn start_live(&self) {
        let session = {
            let st = self.inner.state.borrow();
            if !st.config.live || st.unavailable_gen == Some(st.generation) {
                return;
            }
            LiveSession::new(Rc::clone(&self.inner.feed), st.config.market.clone(), {
                let weak = self.weak();
                Rc::new(move |bar| {
                    if let Some(o) = upgrade(&weak) {
                        o.on_bar(bar);
                    }
                })
            })
        };
        let session = Rc::new(session);
        {
            let mut st = self.inner.state.borrow_mut();
            st.live = Some(Rc::clone(&session));
            // Tab-restore catch-up: browsers throttle hidden tabs (timers to
            // 1/min, sockets can freeze), so bars close unseen while backgrounded.
            // On return, backfill from the last known bar IMMEDIATELY instead of
            // waiting for the next tick to expose the hole (on a quiet symbol that
            // could be many seconds away). The host calls `on_page_visible`.
            st.on_visible = true;
        }
        // Subscribing may deliver a tick at once: no borrow is held.
        session.start();
    }

    /// Stop live ticks + the visibility catch-up and clear the gap-heal state
    /// (switch/destroy).
    pub(super) fn stop_live(&self) {
        let live = {
            let mut st = self.inner.state.borrow_mut();
            st.on_visible = false;
            st.healing = false;
            st.heal_buffer = Vec::new();
            st.last_heal_at = 0.0;
            st.live.take()
        };
        if let Some(live) = live {
            live.stop();
        }
    }

    /// The host's tab-restore signal (the original's `visibilitychange` to
    /// `visible`): backfill from the last known bar right away, unless a heal is
    /// running, the chart cannot heal, or one just ran.
    pub fn on_page_visible(&self) {
        let last = {
            let st = self.inner.state.borrow();
            if !st.on_visible || st.healing {
                return;
            }
            let Some(last) = st.raw_bars.last().copied() else {
                return;
            };
            if self.now() - st.last_heal_at <= HEAL_COOLDOWN_MS {
                return;
            }
            last
        };
        if !self.can_heal() {
            return;
        }
        self.detach(self.heal_gap(last.time));
    }

    /// Re-arm per-load history tracking: resolve the PREVIOUS load's future
    /// (awaiters must never hang on a superseded load), then start a fresh
    /// completed-once cycle.
    pub(super) fn reset_history_tracking(&self) {
        let previous = {
            let mut st = self.inner.state.borrow_mut();
            st.history_completed = false;
            st.history_state = HistoryState::Complete;
            std::mem::replace(&mut st.history_complete, ResolveOnce::new())
        };
        previous.resolve(());
    }

    /// The current market identity: a snapshot of the REQUESTED market, so it
    /// reflects an in-flight `set_market` immediately (the config mutates before
    /// the load).
    pub fn market_snapshot(&self) -> MarketSnapshot {
        let st = self.inner.state.borrow();
        let m = &st.config.market;
        MarketSnapshot {
            symbol: m.symbol.clone(),
            provider: parse_symbol(m.symbol.as_deref().unwrap_or("")).provider,
            timeframe: m.timeframe.clone(),
            bars: m.bars,
            session: m.session,
            offline: m.data.is_some(),
        }
    }

    /// The symbol, timeframe, session and depth of the market as configured now.
    fn market_identity(
        &self,
    ) -> (
        Option<String>,
        Option<String>,
        Option<crate::ports::market_data::MarketSession>,
        Option<usize>,
    ) {
        let st = self.inner.state.borrow();
        let m = &st.config.market;
        (m.symbol.clone(), m.timeframe.clone(), m.session, m.bars)
    }

    /// Switch the chart's market IN PLACE: no destroy/recreate. The renderer
    /// stays mounted (canvases, viewport policy, cosmetic config), panes and
    /// indicator records survive, and user drawings are untouched (per-symbol
    /// drawing documents are a HOST policy via `chart.drawings.to_json()` /
    /// `from_json()`, keyed off `market:changed`). The bars reload through the
    /// shared `load_market` pipeline, then every consumer restarts over the new
    /// market: Pine sessions re-execute, native indicators restart with a fresh
    /// context, the active chart-type data engine is rebuilt, and the live
    /// subscription re-targets.
    ///
    /// Resolves once the new market's history is painted: a deep backfill
    /// continues BEHIND it (await `history_complete` for full depth). A call
    /// superseded by a newer `set_market` (or destroy) resolves silently. Emits
    /// `market:changed` (with `prev`) when the market IDENTITY changed; a
    /// depth-only reload (`bars`) is silent.
    ///
    /// Everything up to the load runs inside the call (the original's `async`
    /// prefix: the generation bump, the quiesce, the config mutation, the first
    /// fetch call). What follows the load runs as a detached task, so the switch
    /// completes even if the caller drops the returned future, as a promise does.
    pub fn set_market(&self, next: MarketSwitch) -> LocalBoxFuture<'static, Result<(), DataError>> {
        let (m_symbol, m_timeframe, m_session, m_bars) = self.market_identity();
        let identity_changed = (next.symbol.is_some() && next.symbol != m_symbol)
            || (next.timeframe.is_some() && next.timeframe != m_timeframe)
            // A session switch changes WHICH bars exist (RTH vs ETH): a full
            // reload, exactly like a timeframe change; the cache keys the
            // sessions apart.
            || (next.session.is_some() && next.session != m_session)
            || next.data.is_some();
        let depth_changed = next.bars.is_some() && next.bars != m_bars;
        // A switch that keeps the symbol and its clock (timeframe, session)
        // carries a running replay over: the new series is cut where the old one
        // stood (see `resume_carried_replay`).
        let same_clock = identity_changed
            && next.data.is_none()
            && (next.symbol.is_none() || next.symbol == m_symbol)
            && (next.timeframe.is_some() || next.session.is_some());
        // A switch landing while an earlier carry still waits for its bars takes
        // the carry over (same clock) or ends it (another symbol); a depth-only
        // reload (the carry's own deepening among them) leaves it be.
        let pending = if identity_changed {
            self.inner.state.borrow().replay_carrying.clone()
        } else {
            None
        };
        if let Some(pending) = &pending {
            self.inner.state.borrow_mut().replay_carrying = None;
            if !same_clock {
                pending.cancelled.set(true);
                self.emit(ChartEvent::ReplayEnd {
                    reason: ReplayEndReason::Market,
                });
            }
        }
        let interval = self.bar_interval_ms();
        let replay_carry = {
            let st = self.inner.state.borrow();
            match (&st.replay_queue, st.raw_bars.last(), &pending) {
                (Some(_), Some(last), _) if same_clock => Some(Rc::new(ReplayCarry {
                    revealed_end: last.time + interval,
                    playing: st.replay_playing,
                    cancelled: Cell::new(false),
                })),
                (_, _, Some(pending)) if same_clock => Some(Rc::new(ReplayCarry {
                    revealed_end: pending.revealed_end,
                    playing: pending.playing,
                    cancelled: Cell::new(false),
                })),
                _ => None,
            }
        };
        if let Some(pending) = &pending {
            // Superseded: whatever it was waiting on is moot.
            pending.cancelled.set(true);
        }
        if let Some(carry) = &replay_carry {
            self.inner.state.borrow_mut().replay_carrying = Some(Rc::clone(carry));
        }
        if replay_carry.is_some() && self.inner.state.borrow().replay_queue.is_some() {
            // Silent: the replay resumes on the new series.
            self.end_replay(
                None,
                EndReplayOpts {
                    restore: false,
                    resume: false,
                },
            );
            let depth = self.inner.state.borrow_mut().replay_depth.take();
            if let Some(depth) = depth
                && next.bars.is_none()
            {
                self.inner.state.borrow_mut().config.market.bars = depth.bars;
            }
        }
        // A reload replaces the history a replay walks. A depth-only change keeps
        // the market, so the full history comes back first and the new depth
        // applies to all of it.
        if self.inner.state.borrow().replay_queue.is_some() && (identity_changed || depth_changed) {
            self.end_replay(
                Some(ReplayEndReason::Market),
                EndReplayOpts {
                    restore: !identity_changed,
                    resume: false,
                },
            );
        }
        // A switch during a start's history load abandons it, and must not load
        // the new market at the depth that start borrowed.
        if self.inner.state.borrow().replay_loading.is_some() && (identity_changed || depth_changed)
        {
            let depth = self.inner.state.borrow().replay_depth.clone();
            self.abandon_replay_load(ReplayEndReason::Market);
            if let Some(depth) = depth
                && next.bars.is_none()
            {
                self.inner.state.borrow_mut().config.market.bars = depth.bars;
            }
        }
        // Same market, only deeper/shallower, with bars already on screen: handled
        // by moving the array's oldest edge instead of reloading (see
        // `extend_depth`). An offline `data` series has no source to extend from,
        // and GROWING needs a ranged feed to fetch the missing older bars: without
        // one the reload is the only way to get them. Shrinking needs no source at
        // all: it is a trim of what is already held.
        let depth_only = {
            let st = self.inner.state.borrow();
            depth_changed
                && !identity_changed
                && !st.raw_bars.is_empty()
                && st.config.market.data.as_ref().is_none_or(|d| d.is_empty())
                && (next.bars.unwrap_or(0) <= st.raw_bars.len()
                    || self.inner.feed.supports_load_range())
        };
        if !identity_changed && !depth_changed {
            // Nothing to reload: honor at most a framing request.
            match next.visible_range {
                Some(InitialRange::Preset(preset)) => self.set_visible_range_preset(preset),
                Some(InitialRange::Range(range)) => self
                    .inner
                    .renderer
                    .set_visible_range(range, VisibleRangeOptions::default()),
                None => {}
            }
            return ready(Ok(()));
        }
        let (prev_symbol, prev_timeframe) = self.market_identity_or_defaults();
        let prev = MarketIdentity {
            symbol: prev_symbol,
            timeframe: prev_timeframe,
        };
        // A SESSION-ONLY flip (RTH<->ETH) changes WHICH bars exist but not the
        // time axis: the user's zoom/position carries over to the reload (captured
        // here, before the quiesce below discards it) unless the caller framed a
        // window of its own. Any other identity change keeps today's re-frame (a
        // new symbol's clock is new). What carries is the VIEW, not the wall clock:
        // the sessions' bar densities differ ~2.5x, so preserving the raw time
        // window would fatten/thin every candle (a perceived zoom change); instead
        // the BAR COUNT on screen and the time of the newest visible bar anchor the
        // reframe against the INCOMING series (see the post-load refinement). The
        // time window still frames the first paint provisionally.
        let session_only = identity_changed
            && next.session.is_some()
            && next.data.is_none()
            && (next.symbol.is_none() || next.symbol == m_symbol)
            && (next.timeframe.is_none() || next.timeframe == m_timeframe);
        let carried = if session_only && next.visible_range.is_none() {
            self.current_visible_range()
        } else {
            None
        };
        // How many bars the user is LOOKING at: the pixel zoom, in series-neutral form.
        let carried_count = carried.map_or(0, |c| {
            self.inner
                .state
                .borrow()
                .raw_bars
                .iter()
                .filter(|b| (b.time as f64) >= c.left && (b.time as f64) <= c.right)
                .count()
        });

        let epoch = self.bump_generation();
        self.inner.state.borrow_mut().switching_market = true;
        // Quiesce the OLD market's inbound flow before touching the config.
        self.stop_live();
        self.clear_viewport_timer();
        // The old market's window means nothing on the new one.
        self.inner.state.borrow_mut().visible_range = None;
        self.reset_history_tracking();

        // Mutate the market in place (the config object is what every reader holds).
        {
            let mut st = self.inner.state.borrow_mut();
            let m = &mut st.config.market;
            if let Some(symbol) = &next.symbol {
                m.symbol = Some(symbol.clone());
            }
            if let Some(timeframe) = &next.timeframe {
                m.timeframe = Some(timeframe.clone());
            }
            if next.bars.is_some() {
                m.bars = next.bars;
            }
            if next.session.is_some() {
                m.session = next.session;
            }
        }
        // Carrying the window is not enough: the incoming series must REACH it, or
        // the viewport clamp throws the user somewhere else (sessions cover very
        // different wall-clock spans for the same bar count: an RTH tape reaches
        // ~2.5x further back than ETH). Bar density is at most one per interval, so
        // this count always covers the window's left edge; capped so a view parked
        // years back never triggers a mega-load (the clamp is the honest fallback
        // there). Deepening only: the user's own depth is never shrunk.
        if let Some(c) = carried {
            let interval = self.bar_interval_ms() as f64;
            let needed = (((self.now() - c.left) / interval).ceil() + 50.0).min(5_000.0);
            let needed = if needed.is_finite() && needed > 0.0 {
                needed as usize
            } else {
                0
            };
            let mut st = self.inner.state.borrow_mut();
            if needed > st.config.market.bars.unwrap_or(500) {
                st.config.market.bars = Some(needed);
            }
        }
        {
            let mut st = self.inner.state.borrow_mut();
            let m = &mut st.config.market;
            if next.data.is_some() {
                m.data = next.data.clone();
            } else if next.symbol.is_some() {
                // Offline to provider switch.
                m.data = None;
            }
            // Consumed by THIS load only (overwritten every switch); a session-only
            // flip re-frames the window the user was looking at.
            m.visible_range = next.visible_range.or_else(|| {
                carried.map(|c| {
                    InitialRange::Range(VisibleRange {
                        from: c.left as Millis,
                        to: c.right as Millis,
                    })
                })
            });
        }

        // An identity switch blanks the chart NOW: the old market's candles must
        // not sit under the new market's name while its bars load: the loading
        // affordance owns the gap. `load:start` goes out first (with the NEW
        // identity, hence after the config mutation) so plugins hide their own
        // visuals before the blank. A depth-only reload keeps the bars (same
        // market, more history).
        if identity_changed {
            self.begin_load(false);
            self.set_bar_series(
                Vec::new(),
                SetBarOpts {
                    clearing: true,
                    ..SetBarOpts::default()
                },
            );
            // Mounted indicator models are stale the moment the identity changes,
            // but their re-runs land only AFTER the new bars paint. Index-aligned
            // series stop on their own (their anchor matches no new bar):
            // time-anchored content (drawings, bgcolor spans) and price-anchored
            // hlines would re-project onto the incoming axis and linger. Blank
            // them all now.
            self.blank_indicator_visuals();
            // The active style's DATA ENGINE still rides the old market: its
            // rebuild only follows the load. Silence it for the gap (its live
            // pushes are stale the moment the identity changed) and blank its
            // channels: per-bar payloads are keyed by bucket open-time, so on a
            // same-timeframe switch the old market's cells would land exactly on
            // the new market's first candles.
            let active = self.inner.state.borrow().active_engine_style.clone();
            if let Some(style) = active {
                let engine = self.inner.state.borrow().type_engines.get(&style).cloned();
                self.suspend_type_engine(engine);
                self.inner.renderer.set_native_data(&style, &Value::Null);
                self.inner
                    .renderer
                    .set_native_data(&format!("{style}-pending"), &json!([]));
            }
            // Native indicators ride the old market too, and a layer payload is
            // just as time-keyed: with no bars on the axis, every entry maps to the
            // same slot. Release them now (the post-load restart builds fresh
            // instances) and blank their channels.
            for record in self.inner.registry.all() {
                if !record.borrow().native.as_ref().is_some_and(|n| n.started) {
                    continue;
                }
                self.stop_native_instance(&record);
                self.inner
                    .renderer
                    .set_native_data(&self.native_channel(&record), &Value::Null);
            }
        }

        let switch = Switch {
            identity_changed,
            depth_only,
            carried,
            carried_count,
            replay_carry,
            prev,
        };
        let done = ResolveOnce::<Result<(), DataError>>::new();

        // A DEPTH-ONLY change is an extension, never a reload: the bars on screen
        // are the same series, so the requested history is a superset (growing) or
        // a suffix (shrinking) of what is already loaded. Re-entering the loader
        // would hand the renderer a fresh short head (a shorter, LATER-starting
        // array) which re-frames the viewport and strands every mounted model's
        // anchor behind the new first bar. Nothing is awaited: the whole switch
        // completes inside this call, as the original's `async` function does.
        if depth_only {
            let requested = self.inner.state.borrow().config.market.bars.unwrap_or(500);
            self.extend_depth(epoch, requested);
            let result = self.finish_set_market(&switch, epoch, Ok(()));
            done.resolve(result);
            return Box::pin(done.wait());
        }

        // Reload through the shared pipeline. The race lets a superseded caller
        // resolve promptly (e.g. a parked load on an unresolvable symbol) instead
        // of hanging. The load starts now (its synchronous prefix) and runs
        // detached, as an abandoned promise does.
        let race = ResolveOnce::<LoadRace>::new();
        let load = eager(self.load_market(epoch, LoadOpts { first_load: false }));
        {
            let race = race.clone();
            self.spawn(Box::pin(async move {
                race.resolve(LoadRace::Loaded(load.await));
            }));
        }
        {
            let race = race.clone();
            self.inner
                .state
                .borrow_mut()
                .supersede_waiters
                .push(Box::new(move || {
                    race.resolve(LoadRace::Superseded);
                }));
        }
        let this = self.clone();
        let finished = done.clone();
        self.spawn(Box::pin(async move {
            let loaded = match js_await(race.wait()).await {
                LoadRace::Loaded(result) => result,
                LoadRace::Superseded => Ok(()),
            };
            let result = this.finish_set_market(&switch, epoch, loaded);
            finished.resolve(result);
        }));
        Box::pin(done.wait())
    }

    /// What `set_market` does once the load has settled (or at once for a
    /// depth-only change).
    fn finish_set_market(
        &self,
        switch: &Switch,
        epoch: u64,
        loaded: Result<(), DataError>,
    ) -> Result<(), DataError> {
        // The original's try/finally: a superseding call set its own flag, so only
        // the owning generation clears it, whether the load succeeded or threw.
        let current = {
            let mut st = self.inner.state.borrow_mut();
            let current = st.generation == epoch;
            if current {
                st.switching_market = false;
            }
            current
        };
        loaded?;
        if !current {
            // Superseded: the newer switch owns the chart.
            return Ok(());
        }
        let interval = self.bar_interval_ms();

        // The carried view's REFINEMENT: the provisional time window framed the
        // first paint, but sessions differ in bar density (~2.5x), so the same wall
        // clock would fatten/thin every candle: a perceived zoom change. Re-derive
        // the window from the bars that actually LOADED: same BAR COUNT on screen
        // (the pixel zoom), the newest visible time as the anchor. Skipped when the
        // old view held no bars (whitespace): the time window is the only truth
        // there.
        if let Some(carried) = switch.carried
            && switch.carried_count >= 2
        {
            let range = {
                let st = self.inner.state.borrow();
                let bars = &st.raw_bars;
                if bars.is_empty() {
                    None
                } else {
                    let mut idx = bars.len() - 1;
                    while idx > 0 && (bars[idx].time as f64) > carried.right {
                        idx -= 1;
                    }
                    let from = bars[idx.saturating_sub(switch.carried_count - 1)].time;
                    let to = carried.right.max((bars[idx].time + 1) as f64) as Millis;
                    Some(VisibleRange { from, to })
                }
            };
            if let Some(range) = range {
                self.inner
                    .renderer
                    .set_visible_range(range, VisibleRangeOptions::default());
            }
        }

        // A carried replay whose point is inside the new bars cuts them right here
        // (no history to wait for: the cut is synchronous); its restart of every
        // consumer over the cut replaces the one below, and live updates stay off.
        // A point OLDER than the new bars has to deepen first: the switch completes
        // as usual and the replay resumes after.
        // Stopped during the load means gone.
        let carry = switch
            .replay_carry
            .clone()
            .filter(|carry| !carry.cancelled.get());
        let carry_from = carry.as_ref().map_or(0, |c| c.revealed_end - interval);
        let carry_now = carry.is_some()
            && self
                .inner
                .state
                .borrow()
                .raw_bars
                .first()
                .is_some_and(|first| carry_from >= first.time);
        let replay_back = match &carry {
            // `replay_start` runs its prefix (the cut) inside the call.
            Some(carry) if carry_now => {
                let start = self.replay_start(ReplayStartOptions { from: carry_from });
                self.resume_carried_replay(start, carry)
            }
            _ => false,
        };

        // Restart every consumer over the new market (records/panes/drawings
        // survive). A depth-only change is NOT a new market: the bars kept their
        // identity and their indices, so tearing the sessions down would recompute
        // everything and flash a stale plot for the whole load. The backfill pokes
        // them per prepend instead: the same path the initial progressive load
        // already uses.
        if !switch.depth_only && !replay_back {
            self.restart_native_indicators();
            self.restart_chart_type_engine();
            self.reexecute_indicators();
        }
        if switch.depth_only {
            self.release_held_natives();
        }
        if !replay_back {
            // Stopped above on every path, depth-only included.
            self.start_live();
        }
        if switch.identity_changed {
            let (symbol, timeframe) = self.market_identity_or_defaults();
            self.emit(ChartEvent::MarketChanged {
                symbol,
                timeframe,
                prev: switch.prev.clone(),
            });
        }
        if let Some(carry) = &carry
            && !carry_now
        {
            let start = self.replay_start(ReplayStartOptions { from: carry_from });
            self.resume_carried_replay(start, carry);
        }
        Ok(())
    }

    /// Stop every chart-type data engine and rebuild the active style's engine
    /// against the new market: engines capture their host (symbol/timeframe) at
    /// start.
    pub(super) fn restart_chart_type_engine(&self) {
        // Stopped while still in the map (a push during `stop` still lands, as in
        // the original), then forgotten.
        let engines: Vec<TypeEngine> = self
            .inner
            .state
            .borrow()
            .type_engines
            .values()
            .cloned()
            .collect();
        for engine in engines {
            engine.borrow_mut().stop();
        }
        let active = {
            let mut st = self.inner.state.borrow_mut();
            st.type_engines.clear();
            st.active_engine_style.clone()
        };
        if let Some(style) = active {
            self.detach(self.start_chart_type_engine(style));
        }
    }

    /// Suspend a chart-type engine. Its host drops every push until the engine
    /// resumes: work it left in flight lands in its own state, never on the scene.
    pub(super) fn suspend_type_engine(&self, engine: Option<TypeEngine>) {
        let Some(engine) = engine else { return };
        engine.borrow_mut().suspend();
        let mut st = self.inner.state.borrow_mut();
        st.suspended_engines.retain(|w| w.strong_count() > 0);
        st.suspended_engines.push(Rc::downgrade(&engine));
    }

    /// The FIRST candles just painted (the quick preview, or the only load):
    /// activate the layers that need bars but NOT full history. The volume layer
    /// reads the chart's bars each frame and a chart-type data engine follows
    /// later chunks/prepends through its own cycle: waiting for the deep chunk
    /// (as the end of init used to) only delayed them.
    pub(super) fn activate_bar_layers(&self) {
        // Auto-add the built-in volume indicator (default on). The initial price
        // style's data requirement is seeded by the constructor; style CHANGES
        // arrive via the renderer's price-style subscription.
        self.maybe_auto_add_volume();
    }

    /// A depth-only reconfigure (`set_market({ bars })` on the same market): move
    /// the array's OLDEST edge, never rebuild it. Growing re-enters the same
    /// backfill loop the initial load uses, so the extra history arrives as
    /// prepends with the viewport preserved; shrinking trims in place. Both keep
    /// the newest bars: the viewport is right-anchored, so what the user is
    /// looking at does not move, and mounted indicators keep their alignment (the
    /// renderer re-derives each model's anchor offset against the new indices,
    /// negative when the head moved forward).
    ///
    /// Sessions are NOT restarted here: the backfill pokes them per prepend
    /// (backfill), and a trim leaves every value they already computed valid.
    fn extend_depth(&self, epoch: u64, requested: usize) {
        let raw_len = self.inner.state.borrow().raw_bars.len();
        if raw_len > requested {
            let trimmed = self.inner.state.borrow().raw_bars[raw_len - requested..].to_vec();
            self.set_bar_series(
                trimmed,
                SetBarOpts {
                    preserve_view: true,
                    ..SetBarOpts::default()
                },
            );
            self.complete_history(HistoryReason::Depth);
            return;
        }
        if raw_len == requested {
            self.complete_history(HistoryReason::Depth);
            return;
        }
        // Detached, exactly like the initial load: `set_market` resolves on what is
        // already painted while the extra depth streams in behind it.
        self.inner.state.borrow_mut().history_state = HistoryState::Backfill;
        self.detach(self.backfill_history(epoch, requested));
    }

    /// The detached backward backfill: fetch chunks strictly older than the
    /// current oldest bar until the requested depth (reason `Depth`), the
    /// source's genesis (`Genesis`: a chunk added nothing older), or a fetch error
    /// (`Aborted`: the chart keeps what loaded). Runs BEHIND the interactive
    /// chart; every prepend keeps the viewport put and pokes the sessions with
    /// `Backfill` so run policy stays with the engines. The generation check after
    /// each await abandons the loop when the chart is superseded.
    fn backfill_history(&self, epoch: u64, requested: usize) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        Box::pin(async move {
            let outcome: Result<(), DataError> = async {
                loop {
                    let (oldest, len) = {
                        let st = this.inner.state.borrow();
                        match st.raw_bars.first() {
                            Some(first) => (first.time, st.raw_bars.len()),
                            None => break,
                        }
                    };
                    if this.generation() != epoch || len >= requested {
                        break;
                    }
                    let remaining = requested - len;
                    // A flat chunk per request: only depths past SINGLE_LOAD_BARS
                    // reach this loop, so there is no small near-window to fill
                    // quickly: every remaining round trip should carry as much as the
                    // source answers well.
                    let step = CHUNK_BARS;
                    let range = BarRange {
                        // Overlap-by-one: the boundary bar dedupes below.
                        to: Some(oldest),
                        limit: Some(step.min(remaining) + 1),
                        ..BarRange::default()
                    };
                    let cfg = this.market_cfg();
                    let chunk = match this.inner.feed.load_range(&cfg, &range) {
                        Some(pending) => js_await(pending).await?,
                        None => {
                            return Err(DataError::other("the feed has no ranged fetch"));
                        }
                    };
                    if this.generation() != epoch {
                        // Superseded mid-fetch (switch/destroy).
                        return Ok(());
                    }
                    // Merge against the CURRENT array (live ticks appended during
                    // the fetch), keeping only genuinely older bars (the existing
                    // boundary bar wins).
                    let (current_oldest, current) = {
                        let st = this.inner.state.borrow();
                        match st.raw_bars.first() {
                            Some(first) => (first.time, st.raw_bars.clone()),
                            None => break,
                        }
                    };
                    let head: Vec<Ohlcv> = chunk
                        .into_iter()
                        .filter(|b| b.time < current_oldest)
                        .collect();
                    if head.is_empty() {
                        // Nothing older exists at the source.
                        this.complete_history(HistoryReason::Genesis);
                        return Ok(());
                    }
                    if head.windows(2).any(|pair| pair[1].time <= pair[0].time) {
                        this.warn(
                            "[vela] non-monotonic history chunk \u{2014} stopping the backfill",
                        );
                        this.complete_history(HistoryReason::Aborted);
                        return Ok(());
                    }
                    let mut merged = head;
                    merged.extend(current);
                    this.set_bar_series(
                        merged,
                        SetBarOpts {
                            preserve_view: true,
                            ..SetBarOpts::default()
                        },
                    );
                    this.notify_sessions_bars(Some(BarsChangeReason::Backfill));
                    let loaded = this.inner.state.borrow().raw_bars.len();
                    this.emit(ChartEvent::HistoryProgress {
                        loaded,
                        target: requested,
                    });
                }
                if this.generation() == epoch {
                    this.complete_history(HistoryReason::Depth);
                }
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                match e {
                    DataError::Unavailable(unavailable) => {
                        this.market_unavailable(epoch, &unavailable);
                    }
                    DataError::Other(message) => {
                        let kept = this.inner.state.borrow().raw_bars.len();
                        this.warn(&format!(
                            "[vela] history backfill failed \u{2014} keeping {kept} bars ({message})"
                        ));
                        if this.generation() == epoch {
                            this.complete_history(HistoryReason::Aborted);
                        }
                    }
                }
            }
        })
    }

    /// Mark the history load finished (exactly once): flip the state, fire the
    /// sessions' `Complete` (their held first run executes now: during a
    /// non-chunked init no session exists yet, so this is a no-op there), emit the
    /// event, resolve the future.
    pub(super) fn complete_history(&self, reason: HistoryReason) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.history_completed {
                return;
            }
            st.history_completed = true;
            st.history_state = HistoryState::Complete;
        }
        self.notify_sessions_bars(Some(BarsChangeReason::Complete));
        let (oldest_time, bars_loaded, latch) = {
            let st = self.inner.state.borrow();
            (
                st.raw_bars.first().map_or(0, |b| b.time),
                st.raw_bars.len(),
                st.history_complete.clone(),
            )
        };
        self.emit(ChartEvent::HistoryComplete {
            reason,
            oldest_time,
            bars_loaded,
        });
        latch.resolve(());
    }

    /// Resolves once the full requested history has loaded (immediately for
    /// small/offline charts). Per load: a market switch re-arms it, resolving the
    /// previous one.
    pub fn history_complete(&self) -> LocalBoxFuture<'static, ()> {
        let latch = self.inner.state.borrow().history_complete.clone();
        Box::pin(async move { latch.wait().await })
    }

    /// The primary symbol as the data layer resolves it, or `None` if no symbol
    /// is set.
    pub(super) fn qualified_symbol(&self) -> Option<String> {
        // The symbol travels BARE. The `provider` option is a preference the feed
        // already applies (it resolves with the chart's provider as first
        // candidate), never a prefix: welding it here made a hard requirement out
        // of it, so metadata and capability probes were aimed at the configured
        // venue even for a symbol only another venue lists: a 502 on the tick-size
        // request and a capability verdict from the wrong venue after every
        // cross-venue switch.
        self.inner
            .state
            .borrow()
            .config
            .market
            .symbol
            .clone()
            .filter(|s| !s.is_empty())
    }

    /// Push the active symbol's tick size to the renderer's price axis. No-op when
    /// unknown (formula stays).
    pub(super) fn refresh_price_format(&self) {
        if !self.inner.renderer.supports_set_price_precision() {
            return;
        }
        let cfg = self.market_cfg();
        let mintick = self
            .inner
            .feed
            .symbol_info(&cfg)
            .and_then(|info| info.extra.get("mintick").and_then(Value::as_f64));
        self.inner
            .renderer
            .set_price_precision(mintick.filter(|m| *m > 0.0));
    }

    /// Resolve the symbol's tick size asynchronously (warming the sync cache) and
    /// push it once it lands.
    pub(super) fn ensure_price_format(&self) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        Box::pin(async move {
            if !this.inner.renderer.supports_set_price_precision() {
                return;
            }
            let Some(symbol) = this.qualified_symbol() else {
                return;
            };
            // Warms the sync metadata cache. Unavailable metadata leaves the
            // renderer on its zoom-derived decimals.
            if js_await(this.inner.data_control.symbol_info(&symbol))
                .await
                .is_ok()
            {
                this.refresh_price_format();
            }
        })
    }

    /// Auto-add the built-in volume indicator, unless the symbol's facts say its
    /// source has no volume (`SymbolInfo::volume == Some(false)`). The load
    /// prefetches those facts beside the bars, so they are usually known by now;
    /// when they are still in flight the add waits for them rather than holding
    /// the first paint, and `pending_native_indicators` reports it.
    pub(super) fn maybe_auto_add_volume(&self) {
        let (enabled, opted_out) = {
            let st = self.inner.state.borrow();
            (st.config.volume == Some(true), st.volume_opted_out)
        };
        // Off, opted out, or no layer.
        if !enabled || opted_out || !self.inner.renderer.supports_set_native_data() {
            return;
        }
        if self.inner.native_registry.borrow().get("volume").is_none() {
            return;
        }
        let cfg = self.market_cfg();
        let known = self.inner.feed.symbol_info(&cfg);
        let symbol = self.qualified_symbol();
        if known.is_none()
            && let Some(symbol) = symbol
            && self.symbol_facts_coming(&symbol)
        {
            let wait = Rc::new(VolumeFactsWait {
                symbol: symbol.clone(),
            });
            self.inner.state.borrow_mut().volume_facts_wait = Some(Rc::clone(&wait));
            let pending = self.inner.data_control.symbol_info(&symbol);
            let this = self.clone();
            self.spawn(Box::pin(async move {
                // `.catch(...).then(...)`: the callback runs two turns after the answer.
                let info = js_await(pending).await.ok().flatten();
                {
                    let mut st = this.inner.state.borrow_mut();
                    // Destroyed meanwhile.
                    if !st
                        .volume_facts_wait
                        .as_ref()
                        .is_some_and(|w| Rc::ptr_eq(w, &wait))
                    {
                        return;
                    }
                    st.volume_facts_wait = None;
                }
                // A switch to another symbol meanwhile drops the add, as a switch
                // before the first candles does: only the first market gets one.
                if this.qualified_symbol().as_deref() != Some(symbol.as_str())
                    || this.inner.state.borrow().volume_opted_out
                {
                    return;
                }
                if info.and_then(|i| i.volume) != Some(false) {
                    this.add_native_indicator("volume", NativeIndicatorOptions::default());
                }
            }));
            return;
        }
        if known.and_then(|i| i.volume) != Some(false) {
            self.add_native_indicator("volume", NativeIndicatorOptions::default());
        }
    }

    /// Whether `symbol`'s facts will still arrive: the registry routes it to a
    /// provider that answers them. Offline data and a custom feed state facts
    /// synchronously or not at all.
    fn symbol_facts_coming(&self, symbol: &str) -> bool {
        if self
            .inner
            .state
            .borrow()
            .config
            .market
            .data
            .as_ref()
            .is_some_and(|d| !d.is_empty())
        {
            return false;
        }
        let dc = &self.inner.data_control;
        dc.resolve(symbol).is_some_and(|resolved| {
            dc.provider_instance(&resolved.provider)
                .is_some_and(|p| p.supports_symbol_info())
        })
    }

    /// The facts in force for the chart's symbol (a copy), `None` while unknown.
    pub fn symbol_facts(&self) -> Option<SymbolFacts> {
        self.inner.state.borrow().facts
    }

    /// Hand the renderer the facts about the market's symbol that decide which
    /// price styles can draw it, and announce a change (`symbol:facts`). The
    /// feed's metadata cache answers first, so a symbol seen before switches in
    /// one step; otherwise the facts read unknown (every style applies) until the
    /// async answer lands: dropped if the chart has moved to another symbol or a
    /// newer probe has started meanwhile.
    pub(super) fn sync_symbol_facts(&self) {
        let probe = {
            let mut st = self.inner.state.borrow_mut();
            st.facts_probe += 1;
            st.facts_probe
        };
        let cfg = self.market_cfg();
        let known = self.inner.feed.symbol_info(&cfg);
        self.apply_symbol_facts(symbol_facts_of(known.as_ref()));
        let symbol = self.qualified_symbol();
        let offline = cfg.data.as_ref().is_some_and(|d| !d.is_empty());
        let Some(symbol) = symbol else { return };
        if known.is_some() || offline {
            return;
        }
        let pending = self.inner.data_control.symbol_info(&symbol);
        let this = self.clone();
        self.spawn(Box::pin(async move {
            // `.catch(...).then(...)`: the callback runs two turns after the answer.
            let info = js_await(pending).await.ok().flatten();
            if this.inner.state.borrow().facts_probe != probe
                || this.qualified_symbol().as_deref() != Some(symbol.as_str())
            {
                return;
            }
            if let Some(info) = info {
                this.apply_symbol_facts(symbol_facts_of(Some(&info)));
            }
        }));
    }

    fn apply_symbol_facts(&self, facts: Option<SymbolFacts>) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.facts == facts {
                return;
            }
            st.facts = facts;
        }
        self.inner.renderer.set_symbol_facts(facts);
        let symbol = self.market_identity_or_defaults().0;
        self.emit(ChartEvent::SymbolFacts {
            symbol,
            facts: self.symbol_facts(),
        });
    }

    /// The native types the chart is about to add on its own: the volume auto-add
    /// while it waits for its symbol's facts. SYNC, for persistence snapshots.
    pub fn pending_native_indicators(&self) -> Vec<String> {
        let (wait, opted_out) = {
            let st = self.inner.state.borrow();
            (st.volume_facts_wait.clone(), st.volume_opted_out)
        };
        let Some(wait) = wait else { return Vec::new() };
        if self.qualified_symbol().as_deref() != Some(wait.symbol.as_str()) || opted_out {
            return Vec::new();
        }
        if self
            .present_native_indicators()
            .iter()
            .any(|t| t == "volume")
        {
            Vec::new()
        } else {
            vec!["volume".to_string()]
        }
    }

    /// Chart-type settings reported by the renderer: remember them and forward
    /// them to the type's engine.
    pub(super) fn on_chart_type_settings(&self, type_id: &str, values: &Map<String, Value>) {
        let engine = {
            let mut st = self.inner.state.borrow_mut();
            st.type_settings.insert(type_id.to_string(), values.clone());
            st.type_engines.get(type_id).cloned()
        };
        if let Some(engine) = engine {
            engine.borrow_mut().on_settings(values);
        }
    }

    /// React to a price-style change. Two styles carry requirements beyond
    /// drawing: a plugin style may need its data engine running, `heikinashi` a
    /// bar-stream transform. Both syncs are idempotent, so the umbrella just fans
    /// out. `None` is a value that is not a style string.
    pub(super) fn sync_price_style(&self, style: Option<&str>) {
        if let Some(style) = style {
            self.inner.state.borrow_mut().price_style = style.to_string();
        }
        self.sync_bar_transform(style);
        self.sync_chart_type_engine(style);
    }

    /// Drive a chart type's DATA engine from the price style: entering a style
    /// whose registered definition carries a data engine starts (or resumes) that
    /// engine; leaving suspends it. Engines are per-chart, created lazily on first
    /// entry, and stopped only at destroy: a style flip is a cheap suspend/resume,
    /// not a rebuild.
    pub(super) fn sync_chart_type_engine(&self, style: Option<&str>) {
        let want = style
            .filter(|id| {
                self.inner
                    .chart_types
                    .borrow()
                    .get(id)
                    .is_some_and(|def| def.data_engine.is_some())
            })
            .map(str::to_string);
        let active = self.inner.state.borrow().active_engine_style.clone();
        if want == active {
            return;
        }
        if let Some(active) = &active {
            let engine = self.inner.state.borrow().type_engines.get(active).cloned();
            self.suspend_type_engine(engine);
        }
        self.inner.state.borrow_mut().active_engine_style = want.clone();
        if let Some(want) = want {
            self.detach(self.start_chart_type_engine(want));
        }
    }

    /// Start (or resume) the active style's data engine, once bars are loaded.
    fn start_chart_type_engine(&self, id: String) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        Box::pin(async move {
            // A renderer without native layers: the style draws plain.
            if !this.inner.renderer.supports_set_native_data() {
                return;
            }
            if js_await(this.inner.ready.wait()).await.is_err() {
                return;
            }
            // The style left again while loading.
            if this.inner.state.borrow().active_engine_style.as_deref() != Some(id.as_str()) {
                return;
            }
            let existing = this.inner.state.borrow().type_engines.get(&id).cloned();
            if let Some(existing) = existing {
                // Before resume(): its re-push must land.
                this.inner
                    .state
                    .borrow_mut()
                    .suspended_engines
                    .retain(|w| w.upgrade().is_some_and(|e| !Rc::ptr_eq(&e, &existing)));
                existing.borrow_mut().resume();
                return;
            }
            let factory = this
                .inner
                .chart_types
                .borrow()
                .get(&id)
                .and_then(|def| def.data_engine.clone());
            let Some(factory) = factory else { return };
            let engine: TypeEngine = Rc::new(RefCell::new(factory()));
            let (stored, market, live) = {
                let mut st = this.inner.state.borrow_mut();
                st.type_engines.insert(id.clone(), Rc::clone(&engine));
                (
                    st.type_settings.get(&id).cloned(),
                    st.config.market.clone(),
                    st.config.live && st.replay_queue.is_none(),
                )
            };
            // Seed the stored settings BEFORE start (a pre-start `on_settings` is
            // pure configuration by contract), so the engine's first fetch already
            // runs on the user's values: never on schema defaults they edited away.
            if let Some(stored) = stored {
                engine.borrow_mut().on_settings(&stored);
            }
            #[allow(clippy::arc_with_non_send_sync)]
            let host: Arc<dyn SeriesDataEngineHost> = Arc::new(TypeEngineHost {
                me: this.weak(),
                data: this.inner.data_control.clone(),
                id: id.clone(),
                engine: Rc::downgrade(&engine),
                symbol: market.symbol.unwrap_or_else(|| "TEST".to_string()),
                timeframe: market.timeframe.unwrap_or_else(|| "60".to_string()),
                live,
                session: market.session.map(|s| s.as_str().to_string()),
            });
            engine.borrow_mut().start(host);
        })
    }

    /// Apply (or drop) the style's bar transform. The transform field flips
    /// SYNCHRONOUSLY: a pre-load style change is picked up by the load's
    /// `set_bar_series`. With bars loaded, the view is rebuilt in place (viewport
    /// preserved) and every Pine indicator re-executes: its input data changed
    /// wholesale, which a streaming session's incremental context cannot absorb.
    /// Natives computed from `ctx.bars()` re-run over the new candles (a synthetic
    /// price style is their source too); layer natives that read the renderer's
    /// bars per frame ignore the poke.
    pub(super) fn sync_bar_transform(&self, style: Option<&str>) {
        let transform = style.and_then(|s| bar_transform_for(&self.inner.chart_types.borrow(), s));
        let view = {
            let mut st = self.inner.state.borrow_mut();
            let same = match (&transform, &st.bar_transform) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            // Singletons per style: identity is enough.
            if same {
                return;
            }
            st.bar_transform = transform.clone();
            if st.raw_bars.is_empty() {
                return;
            }
            st.bars = match &transform {
                Some(t) => t.full(&st.raw_bars),
                None => st.raw_bars.clone(),
            };
            st.bars.clone()
        };
        self.inner.renderer.set_bars(
            &view,
            SetBarsOptions {
                preserve_view: true,
            },
        );
        self.reexecute_indicators();
        if self.inner.state.borrow().switching_market {
            // The restart after the load computes over the new view anyway.
            return;
        }
        for record in self.inner.registry.all() {
            let poke = {
                let r = record.borrow();
                r.native.as_ref().is_some_and(|n| n.started)
                    && r.render_handle.is_some()
                    && !r.hidden
            };
            if poke {
                self.poke_native(&record);
            }
        }
    }

    /// Blank every mounted indicator's painted output (series data, fills,
    /// backgrounds, price lines, drawings, bar colors, trades) while keeping the
    /// mount (legend row, pane, settings) intact, via the same idempotent-by-id
    /// remount an input edit uses. Called on a market identity switch: the fresh
    /// models arrive only after the new bars load, so nothing blanked here is ever
    /// restored: each consumer's re-run remounts a full model over it.
    pub(super) fn blank_indicator_visuals(&self) {
        for record in self.inner.registry.all() {
            let blanked = {
                let r = record.borrow();
                if r.hidden || r.render_handle.is_none() {
                    continue;
                }
                let Some(model) = r.model.as_ref() else {
                    continue;
                };
                blanked_model(model)
            };
            record.borrow_mut().model = Some(blanked.clone());
            let handle = self.inner.renderer.mount_indicator(&blanked);
            {
                let mut r = record.borrow_mut();
                r.render_handle = Some(handle);
                // The next model remounts; never a patch over the blank.
                r.pending_structural = true;
            }
            self.set_loading(&record, true);
        }
    }

    /// Stop + re-run every visible Pine indicator (its input bars changed
    /// wholesale).
    pub(super) fn reexecute_indicators(&self) {
        for record in self.inner.registry.all() {
            let (id, session) = {
                let r = record.borrow();
                match (r.hidden, r.session.clone()) {
                    (false, Some(session)) => (r.id.clone(), session),
                    _ => continue,
                }
            };
            session.stop();
            {
                let mut r = record.borrow_mut();
                r.session = None;
                // Every value changed: full remount, not a patch.
                r.pending_structural = true;
                r.pending_cause = Some(ScriptRunCause::Market);
            }
            self.set_loading(&record, true);
            if let Some(handle) = self.handle_of(&id) {
                self.execute_indicator(&id, &handle);
            }
        }
    }

    /// A live tick from the feed.
    pub(super) fn on_bar(&self, bar: Ohlcv) {
        let heal_from = {
            let mut st = self.inner.state.borrow_mut();
            if st.healing {
                // Replayed once the backfill lands (ordering preserved).
                st.heal_buffer.push(bar);
                return;
            }
            // Continuity check: a bar far ahead of the last one means live updates
            // were MISSED: browsers throttle timers in hidden tabs (and can freeze
            // sockets), so bars close unseen and the poll/stream only ever carries
            // the newest one. Backfill the hole through the feed's ranged fetch
            // instead of pushing a discontinuous bar. Provider-agnostic: every
            // provider serves `getBars({ from })`. Within the cooldown a
            // discontinuity is accepted as a real market gap (an empty interval on
            // an illiquid symbol): no refetch loop.
            let can_heal = self.inner.feed.supports_load_range()
                && st.config.market.data.as_ref().is_none_or(|d| d.is_empty());
            let interval = timeframe_to_ms(st.config.market.timeframe.as_deref().unwrap_or("60"));
            match st.raw_bars.last().copied() {
                Some(last)
                    if can_heal
                        && (bar.time as f64) > last.time as f64 + interval as f64 * GAP_FACTOR
                        && self.now() - st.last_heal_at > HEAL_COOLDOWN_MS =>
                {
                    st.heal_buffer.push(bar);
                    Some(last.time)
                }
                _ => None,
            }
        };
        match heal_from {
            Some(from) => self.detach(self.heal_gap(from)),
            None => self.apply_bar(bar, true),
        }
    }

    /// THE outbound bar seam for a full series load: store the raw series, derive
    /// the view (a copy when no transform is active), and hand the view to the
    /// renderer. Everything downstream (engines via `get_bars`, natives,
    /// visible-range presets) reads the view.
    pub(super) fn set_bar_series(&self, raw: Vec<Ohlcv>, opts: SetBarOpts) {
        let view = {
            let mut st = self.inner.state.borrow_mut();
            let view = match &st.bar_transform {
                Some(t) => t.full(&raw),
                None => raw.clone(),
            };
            st.raw_bars = raw;
            st.bars = view.clone();
            view
        };
        self.inner.renderer.set_bars(
            &view,
            SetBarsOptions {
                preserve_view: opts.preserve_view,
            },
        );
        // The first PAINTED batch ends the loading state: on a deep history that is
        // the quick preview, well before the load pipeline completes. Only a
        // non-empty series counts: the switch-time CLEARING call says so
        // explicitly, and an empty COMPLETED load is closed by `load_market`'s
        // finally instead (a style re-derivation over empty bars mid-load must not
        // end a load still in flight).
        if !opts.clearing && !view.is_empty() {
            self.end_load();
        }
    }

    /// Reconcile one live bar into the canonical array + renderer (no gap
    /// detection). The raw bar lands in `raw_bars`; the VIEW bar is derived
    /// incrementally (a transform needs only the previous view bar: O(1) per
    /// tick) and stays index-aligned with the raw series. The `bar` event carries
    /// the RAW bar (a data event, not a display event). `notify` signals the
    /// indicator sessions too: a heal passes false and notifies ONCE for the whole
    /// batch instead (a session re-run is a full re-execution over the canonical
    /// array, so per-bar notifications during a backfill are pure waste).
    pub(super) fn apply_bar(&self, bar: Ohlcv, notify: bool) {
        let (view_bar, backfill) = {
            let mut st = self.inner.state.borrow_mut();
            let last = st.raw_bars.last().copied();
            let transform = st.bar_transform.clone();
            let view_bar = match last {
                Some(last) if bar.time == last.time => {
                    let slot = st.raw_bars.len() - 1;
                    st.raw_bars[slot] = bar;
                    // The forming candle refined: its values are still provisional.
                    st.bar_cause = ScriptRunCause::Tick;
                    // Replacing the FORMING bar: the previous view bar is the one
                    // BEFORE it.
                    let view_bar = match &transform {
                        Some(t) => {
                            let prev = st.bars.len().checked_sub(2).map(|i| st.bars[i]);
                            t.next(&bar, prev.as_ref())
                        }
                        None => bar,
                    };
                    match st.bars.last_mut() {
                        Some(slot) => *slot = view_bar,
                        None => st.bars.push(view_bar),
                    }
                    view_bar
                }
                Some(last) if bar.time <= last.time => return,
                _ => {
                    st.raw_bars.push(bar);
                    // A new bar opened, so the one before it is now FINAL: the
                    // distinction `script:run` carries as cause `Bar`.
                    st.bar_cause = ScriptRunCause::Bar;
                    let view_bar = match &transform {
                        Some(t) => t.next(&bar, st.bars.last()),
                        None => bar,
                    };
                    st.bars.push(view_bar);
                    view_bar
                }
            };
            (view_bar, st.history_state == HistoryState::Backfill)
        };
        self.inner.renderer.update_bar(&view_bar);
        self.emit(ChartEvent::Bar(bar));
        // During a backfill a tick is tagged backfill too: policy-A sessions hold
        // their run either way, and the eventual complete run reads the tick from
        // the array.
        if notify {
            self.notify_sessions_bars(backfill.then_some(BarsChangeReason::Backfill));
        }
    }

    /// Signal every live session that the bars changed: streamed sessions mark
    /// dirty (re-execute the forming bar), static-live sessions (visible-range
    /// scripts) full-re-run with the new tail. Both value-patch the result. The
    /// reason travels to the ENGINE (which owns run policy): no reason is a live
    /// tick, `Backfill`/`Complete` bracket a history backfill.
    pub(super) fn notify_sessions_bars(&self, reason: Option<BarsChangeReason>) {
        let cause = {
            let st = self.inner.state.borrow();
            if st.switching_market {
                // Old sessions must not run over the incoming market's bars.
                return;
            }
            // A backfill chunk (or the run released by its completion) is still the
            // script's FIRST look at the history, whichever bar arrived last.
            if reason.is_some() {
                ScriptRunCause::History
            } else {
                st.bar_cause
            }
        };
        for record in self.inner.registry.all() {
            let session = {
                let mut r = record.borrow_mut();
                if r.render_handle.is_none() || r.hidden {
                    continue;
                }
                match r.session.clone() {
                    Some(session) => {
                        r.pending_cause = Some(cause);
                        Some(session)
                    }
                    None => None,
                }
            };
            match session {
                Some(session) => session.notify_bars(reason),
                None => {
                    if record.borrow().native.is_some() {
                        self.poke_native(&record);
                    }
                }
            }
        }
        if let Some(engine) = self.active_type_engine() {
            engine.borrow_mut().on_bars();
        }
    }

    /// Tell a native its bars changed. A `waits_for_history` native ignores every
    /// change while the history is still loading (a backfill chunk, a deeper
    /// snapshot, a tick arriving meanwhile) and its output stays on its bars
    /// through the anchor its emit carried. Once the history is complete, a held
    /// one starts and a running one re-runs.
    pub(super) fn poke_native(&self, record: &RecordRef) {
        let (waits, held, started, id) = {
            let r = record.borrow();
            let Some(native) = r.native.as_ref() else {
                return;
            };
            (
                native.descriptor.waits_for_history,
                native.held_for_history,
                native.started,
                r.id.clone(),
            )
        };
        if !waits {
            self.native_on_bars(record);
            return;
        }
        if self.inner.state.borrow().history_state == HistoryState::Backfill {
            return;
        }
        if held {
            if let Some(native) = record.borrow_mut().native.as_mut() {
                native.held_for_history = false;
            }
            if let Some(handle) = self.handle_of(&id) {
                self.detach(self.start_native_indicator(&id, handle));
            }
            return;
        }
        if started {
            self.native_on_bars(record);
        }
    }

    /// Start the natives still held for a history that has completed without
    /// poking them (a depth-only switch finishes its trim while the switch gates
    /// every poke).
    pub(super) fn release_held_natives(&self) {
        if self.inner.state.borrow().history_state == HistoryState::Backfill {
            return;
        }
        for record in self.inner.registry.all() {
            let held = {
                let r = record.borrow();
                !r.hidden && r.native.as_ref().is_some_and(|n| n.held_for_history)
            };
            if held {
                self.poke_native(&record);
            }
        }
    }

    /// Gap healing needs a ranged feed and a provider-backed series (offline
    /// `data` has no source).
    pub(super) fn can_heal(&self) -> bool {
        self.inner.feed.supports_load_range()
            && self
                .inner
                .state
                .borrow()
                .config
                .market
                .data
                .as_ref()
                .is_none_or(|d| d.is_empty())
    }

    /// Backfill missed live bars: re-fetch from the LAST KNOWN bar's open time (so
    /// its stale mid-formation close is corrected too) through "now", and replay
    /// the result, plus any live ticks that arrived during the fetch, through the
    /// normal reconciler. Heals every miss cause the same way: background-tab
    /// throttling, socket reconnects, system sleep, network blips. Run it through
    /// `detach`: its prefix (the healing flag, the fetch call) must run at call
    /// time.
    pub(super) fn heal_gap(&self, from_ms: Millis) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        Box::pin(async move {
            let epoch = this.generation();
            let cfg = {
                let mut st = this.inner.state.borrow_mut();
                st.healing = true;
                st.last_heal_at = this.now();
                st.config.market.clone()
            };
            let range = BarRange {
                from: Some(from_ms),
                ..BarRange::default()
            };
            let result = match this.inner.feed.load_range(&cfg, &range) {
                Some(pending) => js_await(pending).await,
                None => Err(DataError::other("the feed has no ranged fetch")),
            };
            match result {
                Ok(bars) => {
                    // A market switched / chart destroyed mid-heal drops the stale
                    // bars; so does a replay that began mid-heal: these bars lie
                    // past its cursor.
                    let replaying = this.inner.state.borrow().replay_queue.is_some();
                    if this.generation() == epoch && !replaying {
                        for bar in bars {
                            this.apply_bar(bar, false);
                        }
                    }
                }
                // Transient: the buffered ticks still apply; a later discontinuity
                // re-triggers the heal. A permanent refusal ends the market's
                // updates instead.
                Err(DataError::Unavailable(e)) => this.market_unavailable(epoch, &e),
                Err(DataError::Other(_)) => {}
            }
            // A superseded heal leaves the state alone: set_market/destroy already
            // reset it, and replaying its buffer would push the OLD market's bars
            // into the new array.
            let pending = {
                let mut st = this.inner.state.borrow_mut();
                if st.generation == epoch && st.replay_queue.is_none() {
                    st.healing = false;
                    Some(std::mem::take(&mut st.heal_buffer))
                } else {
                    None
                }
            };
            if let Some(pending) = pending {
                for bar in pending {
                    this.apply_bar(bar, false);
                }
                // ONE session notification for the whole heal: bars applied per-bar
                // above (renderer + `bar` events keep their per-bar granularity),
                // but the engines re-execute just once.
                let backfill = this.inner.state.borrow().history_state == HistoryState::Backfill;
                this.notify_sessions_bars(backfill.then_some(BarsChangeReason::Backfill));
            }
        })
    }

    /// A renderer-agnostic snapshot of the graphic elements generated so far (the
    /// deterministic oracle signal).
    pub fn inspect(&self) -> SceneInspection {
        // Native indicators sort to the top of the list (stable otherwise). A
        // record still loading holds only its legend placeholder: it has generated
        // nothing yet, so skip it.
        let mut records: Vec<RecordRef> = self
            .inner
            .registry
            .all()
            .into_iter()
            .filter(|r| !r.borrow().loading)
            .collect();
        records.sort_by_key(|r| r.borrow().native.is_none());
        let models: Vec<IndicatorModel> = records
            .iter()
            .filter_map(|r| r.borrow().model.clone())
            .collect();
        inspect_models(&models)
    }

    pub fn resize(&self) {
        self.inner.renderer.resize();
    }

    /// Swap the app theme live: re-skins the renderer (surfaces, axes, legends,
    /// in-chart chrome) and emits `theme:changed` so host chrome around the chart
    /// follows. No-ops when the resolved theme is already active, which also
    /// breaks the echo when a host reacts to `theme:changed` by calling back into
    /// `set_theme`. (The comparison is deep equality of the theme value; the
    /// original compares each top-level key with `===`.)
    pub fn set_theme(&self, theme: VelaTheme) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.config.theme == theme {
                return;
            }
            st.config.theme = theme.clone();
        }
        self.inner.renderer.set_theme(&theme);
        self.emit(ChartEvent::ThemeChanged(theme));
    }

    /// The current visible time range (left/right bar times), or `None` before
    /// data loads.
    pub fn get_visible_range(&self) -> Option<VisibleRange> {
        self.inner.renderer.get_visible_range()
    }

    /// Set the visible time range explicitly (epoch-ms `from`/`to`).
    pub fn set_visible_range(&self, range: VisibleRange) {
        self.inner
            .renderer
            .set_visible_range(range, VisibleRangeOptions::default());
    }

    /// Pan by a fraction of the visible width (positive: toward the latest bars).
    /// Prefers the renderer's drag-equivalent pan; falls back to an instant shift
    /// of the visible range for renderers without one.
    pub fn pan_by(&self, fraction: f64) {
        if self.inner.renderer.supports_pan_by() {
            self.inner.renderer.pan_by(fraction);
            return;
        }
        let Some(range) = self.inner.renderer.get_visible_range() else {
            return;
        };
        let delta = (range.to - range.from) as f64 * fraction;
        self.inner.renderer.set_visible_range(
            VisibleRange {
                from: (range.from as f64 + delta).round() as Millis,
                to: (range.to as f64 + delta).round() as Millis,
            },
            VisibleRangeOptions::default(),
        );
    }

    /// Frame a named preset (e.g. `1M`, `YTD`, `ALL`) over the loaded bars. Every
    /// preset ends on the newest bar, so the frame keeps the configured right
    /// margin past it (as scroll-to-latest and the first fit do): "Reset chart
    /// view" frames `ALL`.
    pub fn set_visible_range_preset(&self, preset: VisibleRangePreset) {
        let range = preset_to_range(preset, &self.inner.state.borrow().bars);
        if let Some(range) = range {
            self.inner
                .renderer
                .set_visible_range(range, VisibleRangeOptions { right_margin: true });
        }
    }

    /// Adopt the chart's translator (one object for the chart's lifetime).
    pub fn set_translator(&self, i18n: Rc<dyn Translator>) {
        self.inner.state.borrow_mut().i18n = Some(i18n);
        // The toolbar went out before the chart's translator arrived.
        self.inner.drawings.relocalize();
    }

    /// The chart's language changed: the drawing toolbar's labels and native
    /// indicators' names, as the handles, the pane listing and script runs report
    /// them, follow it. The renderer re-reads the display names it shows on its
    /// own.
    pub fn relocalize(&self) {
        self.inner.drawings.relocalize();
        let i18n = self.translator();
        for record in self.inner.registry.all() {
            let (id, title, short_title) = {
                let r = record.borrow();
                let Some(native) = r.native.as_ref() else {
                    continue;
                };
                (
                    r.id.clone(),
                    native.descriptor.title.clone(),
                    native.descriptor.short_title.clone(),
                )
            };
            let title = text_of(i18n.as_deref(), &title, &[]);
            let short = short_title.map(|m| text_of(i18n.as_deref(), &m, &[]));
            {
                let mut r = record.borrow_mut();
                r.title = title.clone();
                if let Some(model) = r.model.as_mut() {
                    model.title = title.clone();
                    if let Some(short) = short {
                        model.shorttitle = Some(short);
                    }
                }
            }
            if let Some(handle) = self.handle_of(&id) {
                handle.set_title(title);
            }
        }
    }

    pub fn destroy(&self) {
        // Abandon backfill loops/loads mid-flight + release superseded
        // `set_market` awaiters.
        self.bump_generation();
        let history = {
            let mut st = self.inner.state.borrow_mut();
            st.destroyed = true;
            st.volume_facts_wait = None;
            // A facts answer landing after destroy must not reach the renderer.
            st.facts_probe += 1;
            st.history_complete.clone()
        };
        // Never leave `history_complete()` awaiters hanging.
        history.resolve(());
        self.clear_replay_timer();
        {
            let mut st = self.inner.state.borrow_mut();
            st.replay_queue = None;
            st.replay_forming = None;
            st.replay_carrying = None;
        }
        self.drop_tick_fetches();
        {
            let mut st = self.inner.state.borrow_mut();
            st.replay_depth = None;
            st.replay_loading = None;
        }
        self.stop_live();
        self.clear_viewport_timer();
        let unsubs = {
            let mut st = self.inner.state.borrow_mut();
            [
                st.viewport_unsub.take(),
                st.pane_action_unsub.take(),
                st.price_style_will_change_unsub.take(),
            ]
        };
        for unsub in unsubs.into_iter().flatten() {
            unsub();
        }
        // Native instances free their own caches/timers in stop().
        for record in self.inner.registry.all() {
            self.settle_code(
                &record,
                CodeUpdateResult::failure("[vela] updateCode \u{2014} the chart was destroyed."),
            );
            let session = record.borrow().session.clone();
            if let Some(session) = session {
                session.stop();
            }
            if record.borrow().native.is_some() {
                self.stop_native_instance(&record);
            }
        }
        // Chart-type data engines (SDK).
        let engines: Vec<TypeEngine> = {
            let st = self.inner.state.borrow();
            st.type_engines.values().cloned().collect()
        };
        for engine in engines {
            engine.borrow_mut().stop();
        }
        let unresolved = {
            let mut st = self.inner.state.borrow_mut();
            st.type_engines.clear();
            st.active_engine_style = None;
            st.unresolved_unsub.take()
        };
        if let Some(unsub) = unresolved {
            unsub();
        }
        // Parked waits would otherwise outlive the chart.
        self.inner.feed.destroy();
        self.inner.drawings.destroy();
        self.inner.marks.destroy();
        self.inner.renderer.destroy();
        self.inner.events.clear();
    }
}

/// The style-deciding facts in a symbol's info: `None` while there is no info
/// (unknown), an empty record for info that states none (every style applies).
fn symbol_facts_of(info: Option<&crate::ports::market_data::SymbolInfo>) -> Option<SymbolFacts> {
    info.map(|i| SymbolFacts {
        ohlc: i.ohlc,
        volume: i.volume,
    })
}

/// A copy of a mounted model with every painted output emptied. The structure
/// (series specs: ids, titles, styles; pane routing; inputs) survives, so an
/// idempotent remount keeps the legend and settings while nothing stale paints.
/// The anchor goes: nothing index-aligned is left to place, and the old market's
/// bar time matches none of the incoming bars (the renderer would warn on every
/// switch).
fn blanked_model(model: &IndicatorModel) -> IndicatorModel {
    let mut blanked = model.clone();
    blanked.anchor_time = None;
    for spec in &mut blanked.series {
        match &mut spec.data {
            SeriesData::LineLike { points, .. } => points.clear(),
            SeriesData::Candle { bars, .. } => bars.clear(),
            SeriesData::Markers { markers } => markers.clear(),
        }
    }
    blanked.fills.clear();
    blanked.backgrounds.clear();
    blanked.price_lines.clear();
    blanked.lines.clear();
    blanked.boxes.clear();
    blanked.labels.clear();
    blanked.polylines.clear();
    blanked.linefills.clear();
    blanked.tables.clear();
    blanked.bar_colors.clear();
    blanked.trades.clear();
    blanked
}

/// The environment a chart hands to a style's data engine: a snapshot of the
/// market at start (an engine is rebuilt on every market or replay change, so it
/// never changes within one host's lifetime) and the live bars. Holds the chart
/// weakly. Pushes reach the scene only from the chart's current, unsuspended
/// engine: an engine stopped by a rebuild (market switch, replay) or suspended
/// mid-fetch still holds this host, and its work lands nowhere.
struct TypeEngineHost {
    me: Weak<Inner>,
    /// The chart's `DataControl` (the original hands over `this.dataControl`).
    data: DataControl,
    id: String,
    engine: Weak<RefCell<Box<dyn crate::chart_types::SeriesDataEngine>>>,
    symbol: String,
    timeframe: String,
    live: bool,
    session: Option<String>,
}

impl TypeEngineHost {
    fn reaches_scene(&self, o: &EngineOrchestrator) -> bool {
        let Some(engine) = self.engine.upgrade() else {
            return false;
        };
        let st = o.inner.state.borrow();
        st.type_engines
            .get(&self.id)
            .is_some_and(|current| Rc::ptr_eq(current, &engine))
            && !st
                .suspended_engines
                .iter()
                .any(|w| w.upgrade().is_some_and(|e| Rc::ptr_eq(&e, &engine)))
    }
}

impl SeriesDataEngineHost for TypeEngineHost {
    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn timeframe(&self) -> &str {
        &self.timeframe
    }

    fn live(&self) -> bool {
        self.live
    }

    fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn bars(&self) -> Vec<Ohlcv> {
        self.me
            .upgrade()
            .map(|inner| inner.state.borrow().bars.clone())
            .unwrap_or_default()
    }

    fn push_data(&self, data: Value) {
        if let Some(o) = upgrade(&self.me)
            && self.reaches_scene(&o)
        {
            o.inner.renderer.set_native_data(&self.id, &data);
        }
    }

    fn push_pending(&self, ranges: &[(i64, i64)]) {
        if let Some(o) = upgrade(&self.me)
            && self.reaches_scene(&o)
        {
            let ranges: Vec<Value> = ranges.iter().map(|(a, b)| json!([a, b])).collect();
            o.inner
                .renderer
                .set_native_data(&format!("{}-pending", self.id), &Value::Array(ranges));
        }
    }

    fn data(&self) -> DataControl {
        self.data.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use serde_json::json;

    use super::super::test_support::*;
    use super::*;
    use crate::chart_types::{ChartTypeDefinition, SeriesDataEngine};
    use crate::data_unavailable::{DataUnavailableDetails, DataUnavailableReason};
    use crate::events::types::ChartEventName;
    use crate::indicators::VisibleRange;
    use crate::ports::market_data::{MarketSession, SymbolInfo};
    use crate::price_styles::{BarTransform, heikin_ashi_full};
    use crate::theme::{dark_theme, light_theme};

    // ── helpers ──

    /// `(n, preserve_view)` of every `set_bars` call.
    fn calls(rig: &Rig) -> Vec<(usize, bool)> {
        rig.renderer
            .set_bars_calls
            .borrow()
            .iter()
            .map(|c| (c.n, c.preserve_view))
            .collect()
    }

    fn rig_with_prices() -> RigBuilder {
        Rig::builder().feed(|f| {
            f.set_price("AAA", 100.0);
            f.set_price("BBB", 200.0);
            f.set_price("CCC", 300.0);
            f.set_price("SLOW", 900.0);
        })
    }

    fn close0(rig: &Rig) -> Option<f64> {
        rig.renderer.bars.borrow().first().map(|b| b.close)
    }

    fn market_changed(rig: &Rig) -> Vec<(String, String, MarketIdentity)> {
        rig.log
            .of(ChartEventName::MarketChanged)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::MarketChanged {
                    symbol,
                    timeframe,
                    prev,
                } => Some((symbol, timeframe, prev)),
                _ => None,
            })
            .collect()
    }

    fn completions(rig: &Rig) -> Vec<(HistoryReason, Millis, usize)> {
        rig.log
            .of(ChartEventName::HistoryComplete)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::HistoryComplete {
                    reason,
                    oldest_time,
                    bars_loaded,
                } => Some((reason, oldest_time, bars_loaded)),
                _ => None,
            })
            .collect()
    }

    fn bar_times(rig: &Rig) -> Vec<Millis> {
        rig.log
            .of(ChartEventName::Bar)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::Bar(b) => Some(b.time),
                _ => None,
            })
            .collect()
    }

    /// `(what, timeframe, bars painted, end payload bars, first_load)` of a load event.
    type LoadSeen = (String, String, usize, Option<usize>, Option<bool>);

    fn switch_to(rig: &Rig, next: MarketSwitch) -> Result<(), DataError> {
        rig.switch(next)
    }

    // ── load pipeline ──

    #[test]
    fn the_first_load_raises_the_affordance_paints_and_ends_it() {
        let rig = Rig::builder().build();
        // The constructor's synchronous prefix raised the affordance (and fired
        // `load:start`, before any host could subscribe); nothing landed yet.
        assert_eq!(*rig.renderer.timeline.borrow(), ["loading:true"]);
        assert!(rig.log.names().is_empty());
        rig.ready().expect("the first load succeeds");
        assert_eq!(
            *rig.renderer.timeline.borrow(),
            ["loading:true", "bars:50", "loading:false"]
        );
        // The first painted batch ends the loading state, the completion follows.
        assert_eq!(rig.log.names(), ["load:end", "history:complete", "ready"]);
        assert_eq!(
            rig.renderer.mounted_with.borrow().clone(),
            Some(dark_theme())
        );
    }

    #[test]
    fn nothing_lands_before_the_constructor_returns_and_a_listener_attached_after_sees_the_load_in_order()
     {
        // An immediately answering feed (a cache hit, an offline `data` array, any
        // fake): in JavaScript `await` still yields, so the first load lands a turn
        // after the constructor, and a host that subscribes right after
        // constructing the chart sees it.
        for offline in [false, true] {
            let builder = Rig::builder();
            let rig = if offline {
                builder
                    .data(crate::engine::orchestrator::test_support::bars(30))
                    .build()
            } else {
                builder.build()
            };
            // The synchronous prefix ran: the affordance is up and the first fetch
            // was asked for.
            assert_eq!(*rig.renderer.timeline.borrow(), ["loading:true"]);
            assert_eq!(rig.feed.loads().len(), 1);
            // Nothing else: no bars, no `load:end`, no `history:complete`.
            assert!(rig.renderer.bars.borrow().is_empty());
            assert!(rig.log.names().is_empty());
            rig.settle();
            assert_eq!(
                rig.log.names(),
                ["load:end", "history:complete", "ready"],
                "offline: {offline}"
            );
            assert_eq!(
                rig.renderer.bars.borrow().len(),
                if offline { 30 } else { 50 }
            );
        }
    }

    #[test]
    fn an_async_call_returns_to_its_caller_at_its_first_await() {
        // `set_market` reloads: its synchronous prefix (generation, blank, the
        // first fetch call) runs in the call; the answer, though ready at once,
        // lands a turn later.
        let rig = Rig::builder().build();
        rig.loaded();
        let loads_before = rig.feed.loads().len();
        let done = rig.chart.set_market(to_symbol("BBB"));
        assert_eq!(rig.feed.loads().len(), loads_before + 1);
        assert!(
            rig.renderer.bars.borrow().is_empty(),
            "blanked, not repainted yet"
        );
        assert!(rig.chart.inner.state.borrow().switching_market);
        rig.settle();
        assert_eq!(rig.renderer.bars.borrow().len(), 50);
        assert!(!rig.chart.inner.state.borrow().switching_market);
        rig.block_on(done).expect("switch");
    }

    #[test]
    fn a_depth_only_switch_returns_with_the_extension_still_streaming_behind_it() {
        let rig = Rig::builder()
            .bars(100)
            .feed(|f| f.set_depth("AAA", 900))
            .build();
        rig.loaded();
        let done = rig.chart.set_market(MarketSwitch {
            bars: Some(400),
            ..MarketSwitch::default()
        });
        // The whole switch ran in the call (no await in the original's depth-only
        // path), except the backfill's continuation after its first await.
        assert!(!rig.chart.inner.state.borrow().switching_market);
        assert_eq!(rig.renderer.bars.borrow().len(), 100);
        assert_eq!(rig.feed.ranges().len(), 1, "the first chunk was asked for");
        let complete = rig.chart.history_complete();
        rig.block_on(complete);
        assert_eq!(rig.renderer.bars.borrow().len(), 400);
        rig.block_on(done).expect("switch");
    }

    #[test]
    fn an_ordinary_depth_arrives_in_one_request() {
        let rig = Rig::builder()
            .bars(2000)
            .feed(|f| f.default_depth.set(30_000))
            .build();
        rig.loaded();
        // Splitting this range only ever bought a faster FIRST candle, and nothing
        // downstream can use it: an indicator's first run is held until the full depth.
        assert_eq!(calls(&rig), [(2000, false)]);
        assert_eq!(rig.feed.loads().len(), 1);
    }

    #[test]
    fn the_single_request_boundary_is_5000_and_5001_is_still_one_round_trip() {
        for requested in [5000, 5001] {
            let rig = Rig::builder()
                .bars(requested)
                .feed(|f| f.default_depth.set(30_000))
                .build();
            rig.loaded();
            // Past the line the load is chunked, but the first chunk is 10 000 bars.
            assert_eq!(calls(&rig), [(requested, false)], "bars {requested}");
            assert!(rig.feed.ranges().is_empty());
        }
    }

    #[test]
    fn a_small_request_and_a_mid_sized_one_load_in_one_shot_and_complete_with_depth() {
        let rig = Rig::builder()
            .bars(500)
            .feed(|f| f.default_depth.set(30_000))
            .build();
        rig.loaded();
        assert_eq!(calls(&rig), [(500, false)]);
        let oldest = rig.renderer.bars.borrow()[0].time;
        assert_eq!(completions(&rig), [(HistoryReason::Depth, oldest, 500)]);

        let small = Rig::builder()
            .bars(200)
            .feed(|f| f.default_depth.set(30_000))
            .build();
        small.loaded();
        assert_eq!(calls(&small), [(200, false)]);
    }

    #[test]
    fn a_requested_initial_window_loads_in_one_pass_and_is_framed_with_the_first_paint() {
        let rig = Rig::builder()
            .bars(2000)
            .visible_range(InitialRange::Preset(VisibleRangePreset::Day1))
            .feed(|f| f.default_depth.set(30_000))
            .build();
        rig.loaded();
        // No preview pass: its recent-bars window would paint the WRONG range for a moment.
        assert_eq!(calls(&rig), [(2000, false)]);
        let framed = rig.renderer.visible_range_calls.borrow().clone();
        assert_eq!(framed.len(), 1);
        assert_eq!(framed[0].0.to - framed[0].0.from, 86_400_000);
        // A preset keeps the configured right margin.
        assert!(framed[0].1.right_margin);
        // The frame was set before anything else could be painted over it.
        assert_eq!(
            *rig.renderer.timeline.borrow(),
            ["loading:true", "bars:2000", "loading:false"]
        );
    }

    #[test]
    fn an_explicit_initial_range_is_framed_as_given() {
        let range = VisibleRange {
            from: 1_000,
            to: 5_000,
        };
        let rig = Rig::builder()
            .bars(2000)
            .visible_range(InitialRange::Range(range))
            .feed(|f| f.default_depth.set(30_000))
            .build();
        rig.loaded();
        let framed = rig.renderer.visible_range_calls.borrow().clone();
        assert_eq!(framed.len(), 1);
        assert_eq!(framed[0].0, range);
        assert!(!framed[0].1.right_margin);
    }

    #[test]
    fn a_rangeless_feed_keeps_the_preview_then_full_shape() {
        let rig = Rig::builder()
            .bars(8000)
            .ranged(false)
            .feed(|f| f.default_depth.set(10_000))
            .build();
        rig.loaded();
        // Stepping would re-download the head every time.
        assert_eq!(calls(&rig), [(PREVIEW_BARS, false), (8000, true)]);
        assert!(rig.feed.ranges().is_empty());
        let asked: Vec<_> = rig.feed.loads().iter().map(|c| c.bars).collect();
        assert_eq!(asked, [Some(300), Some(8000)]);
    }

    #[test]
    fn a_deep_chart_paints_a_first_chunk_then_streams_flat_10k_steps_bounded_by_the_remainder() {
        let rig = Rig::builder()
            .bars(25_000)
            .feed(|f| {
                f.default_depth.set(40_000);
                // Park the backfill so the interactive intermediate state is observable.
                f.gate_ranges(true);
            })
            .build();
        rig.ready().expect("ready at the first paint");
        assert_eq!(calls(&rig), [(10_000, false)]);

        rig.feed.gate_ranges(false);
        rig.feed.release();
        rig.loaded();
        // Flat chunks behind the interactive chart, the last one bounded by the remainder.
        assert_eq!(
            calls(&rig),
            [(10_000, false), (20_000, true), (25_000, true)]
        );
        let progress: Vec<_> = rig
            .log
            .of(ChartEventName::HistoryProgress)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::HistoryProgress { loaded, target } => Some((loaded, target)),
                _ => None,
            })
            .collect();
        assert_eq!(progress, [(20_000, 25_000), (25_000, 25_000)]);
        let oldest = rig.renderer.bars.borrow()[0].time;
        assert_eq!(completions(&rig), [(HistoryReason::Depth, oldest, 25_000)]);
        // Requested backward, overlap-by-one, bounded by the remaining depth.
        let limits: Vec<_> = rig.feed.ranges().iter().map(|r| r.limit).collect();
        assert_eq!(limits, [Some(10_001), Some(5_001)]);
        // Strictly monotonic across every seam.
        let bars = rig.renderer.bars.borrow();
        assert!(bars.windows(2).all(|w| w[1].time > w[0].time));
    }

    #[test]
    fn the_backfill_stops_at_genesis_when_a_chunk_adds_nothing_older() {
        let rig = Rig::builder()
            .bars(25_000)
            .feed(|f| f.default_depth.set(12_000))
            .build();
        rig.loaded();
        assert_eq!(rig.renderer.bars.borrow().len(), 12_000);
        let oldest = rig.renderer.bars.borrow()[0].time;
        assert_eq!(
            completions(&rig),
            [(HistoryReason::Genesis, oldest, 12_000)]
        );
    }

    #[test]
    fn a_failing_backfill_keeps_the_loaded_bars_and_completes_aborted() {
        let rig = Rig::builder()
            .bars(25_000)
            .feed(|f| {
                f.default_depth.set(40_000);
                f.set_range_error(Some(DataError::other("backfill fetch failed")));
            })
            .build();
        rig.loaded();
        assert_eq!(rig.renderer.bars.borrow().len(), 10_000);
        let completed = completions(&rig);
        assert_eq!(completed.len(), 1);
        assert_eq!(
            (completed[0].0, completed[0].2),
            (HistoryReason::Aborted, 10_000)
        );
        assert!(
            rig.platform
                .warnings()
                .iter()
                .any(|w| w.contains("history backfill failed") && w.contains("keeping 10000 bars"))
        );
    }

    #[test]
    fn destroy_mid_backfill_abandons_the_loop_and_resolves_history_complete() {
        let rig = Rig::builder()
            .bars(25_000)
            .feed(|f| {
                f.default_depth.set(40_000);
                f.gate_ranges(true);
            })
            .build();
        rig.ready().expect("ready");
        assert_eq!(rig.renderer.bars.borrow().len(), 10_000);

        rig.chart.destroy();
        // The in-flight step lands AFTER destroy: it must be discarded.
        rig.feed.release();
        let complete = rig.chart.history_complete();
        rig.block_on(complete);
        rig.settle();
        assert_eq!(rig.renderer.set_bars_calls.borrow().len(), 1);
        assert!(completions(&rig).is_empty(), "no event after destroy");
    }

    #[test]
    fn a_non_monotonic_chunk_stops_the_backfill() {
        // The universe's newest 10 000 bars paint first, so its oldest sits 30 000 hours in.
        let oldest = T0 + 30_000 * HOUR;
        let rig = Rig::builder()
            .bars(25_000)
            .feed(move |f| {
                f.default_depth.set(40_000);
                // Two "older" bars, out of order.
                f.set_range_result(Some(vec![
                    bar_at(oldest - 2 * HOUR, 1.0),
                    bar_at(oldest - 3 * HOUR, 1.0),
                ]));
            })
            .build();
        rig.loaded();
        assert_eq!(rig.renderer.bars.borrow().len(), 10_000);
        assert_eq!(completions(&rig)[0].0, HistoryReason::Aborted);
        assert!(
            rig.platform
                .warnings()
                .iter()
                .any(|w| w.contains("non-monotonic"))
        );
    }

    // ── progressive loads ──

    #[test]
    fn a_progressive_feed_paints_every_snapshot_and_completes_when_it_resolves() {
        let all = bars(120);
        let rig = Rig::builder().bars(120).progressive(true).build();
        assert_eq!(rig.feed.stream_count(), 1);
        // Below the 20-bar first-paint hold: the frame it would set is unusable.
        rig.feed.emit_batch(all[110..].to_vec());
        assert!(calls(&rig).is_empty());
        // Deep enough to carry the framing: paints, the load resolves.
        rig.feed.emit_batch(all[80..].to_vec());
        rig.ready().expect("ready at the first paint");
        assert_eq!(calls(&rig), [(40, false)]);
        // A deeper snapshot repaints with the viewport preserved.
        rig.feed.emit_batch(all[10..].to_vec());
        assert_eq!(calls(&rig), [(40, false), (110, true)]);
        // The final answer: last paint + completion.
        rig.feed.finish_progressive(Ok(Some(all.clone())));
        rig.loaded();
        assert_eq!(calls(&rig), [(40, false), (110, true), (120, true)]);
        assert_eq!(completions(&rig)[0].0, HistoryReason::Depth);
        assert!(rig.feed.loads().is_empty(), "the classic load never ran");
    }

    #[test]
    fn a_progressive_none_falls_back_to_the_classic_load() {
        let rig = Rig::builder()
            .bars(30)
            .progressive(true)
            .feed(|f| f.set_depth("AAA", 30))
            .build();
        rig.feed.finish_progressive(Ok(None));
        rig.loaded();
        assert_eq!(calls(&rig), [(30, false)]);
        assert_eq!(rig.feed.loads().len(), 1);
    }

    #[test]
    fn a_progressive_final_answer_below_the_first_paint_hold_still_paints() {
        let rig = Rig::builder().bars(500).progressive(true).build();
        // Held: a frame set from this would be unusable.
        rig.feed.emit_batch(bars(10));
        rig.feed.finish_progressive(Ok(Some(bars(15))));
        rig.loaded();
        // All the history there is: the final answer paints whatever exists.
        assert_eq!(calls(&rig), [(15, false)]);
        assert_eq!(completions(&rig)[0].0, HistoryReason::Genesis);
    }

    #[test]
    fn a_progressive_error_completes_aborted_and_an_unavailable_one_reports_the_market() {
        let rig = Rig::builder().bars(500).progressive(true).build();
        rig.feed.emit_batch(bars(40));
        rig.feed
            .finish_progressive(Err(DataError::other("poll failed")));
        rig.loaded();
        assert_eq!(completions(&rig)[0].0, HistoryReason::Aborted);
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 0);

        let rig = Rig::builder().bars(500).progressive(true).build();
        rig.feed.finish_progressive(Err(unavailable("AAA", "60")));
        rig.loaded();
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 1);
    }

    #[test]
    fn switching_markets_aborts_the_in_flight_progressive_stream() {
        // An abandoned stream left polling its budget out starves the connection
        // pool, and the NEXT symbol's very first fetch with it.
        let rig = Rig::builder().bars(100).progressive(true).build();
        let first = rig.feed.stream_signal(0).expect("a signal");
        assert!(!first.aborted());
        rig.feed.emit_batch(bars(100)); // the first paint releases the pipeline
        rig.ready().expect("ready");

        let switching = rig.chart.set_market(to_symbol("OTHER"));
        // Everything up to the new load ran inside the call.
        assert!(first.aborted(), "the old stream was told to stop, promptly");
        assert_eq!(rig.feed.stream_count(), 2);
        let second = rig.feed.stream_signal(1).expect("a fresh signal");
        assert!(!second.aborted());

        rig.feed.emit_batch(bars(100)); // the new stream paints: the switch resolves
        rig.block_on(switching).expect("the switch resolves");

        // The abandoned stream delivering late changes nothing.
        let before = calls(&rig);
        rig.feed.emit_batch_on(0, bars(60));
        rig.feed.finish_progressive_on(0, Ok(Some(bars(100))));
        rig.settle();
        assert_eq!(calls(&rig), before);
    }

    #[test]
    fn a_progressive_snapshot_during_a_replay_is_merged_not_painted() {
        // The guard is here; the merge itself is tested in the replay section.
        let rig = Rig::builder().bars(120).progressive(true).build();
        rig.feed.emit_batch(bars(120)[80..].to_vec());
        rig.ready().expect("ready");
        rig.chart.inner.state.borrow_mut().replay_queue = Some(VecDeque::new());
        let before = calls(&rig);
        rig.feed.emit_batch(bars(120)[10..].to_vec());
        // Not painted as a fresh load: the 70 older bars join the cut's head, view kept.
        let mut expected = before.clone();
        expected.push((110, true));
        assert_eq!(calls(&rig), expected);
    }

    // ── live bars and gap healing ──

    fn gap_rig() -> Rig {
        let rig = Rig::builder()
            .live(true)
            .feed(|f| f.default_depth.set(10))
            .build();
        rig.ready().expect("ready");
        rig
    }

    #[test]
    fn heals_a_live_bar_gap_with_a_ranged_backfill_from_the_last_known_bar() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        // The backfill the provider would return: the last known bar (corrected), the
        // two missed bars, and the forming one.
        rig.feed.set_range_result(Some(
            (0..4).map(|k| bar_at(last + k * HOUR, 100.0)).collect(),
        ));
        // A live tick lands 3 intervals ahead: bars closed unseen.
        rig.feed.push_tick(bar_at(last + 3 * HOUR, 999.0));
        rig.settle();

        let ranges = rig.feed.ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            ranges[0].from,
            Some(last),
            "the re-fetch includes the last known bar"
        );
        // The missed bars in order, then the buffered live tick replayed on top.
        assert_eq!(
            bar_times(&rig),
            [
                last,
                last + HOUR,
                last + 2 * HOUR,
                last + 3 * HOUR,
                last + 3 * HOUR
            ]
        );
        let raw = rig.raw();
        assert_eq!(raw.len(), 13);
        assert_eq!(raw[12].close, 999.0, "the buffered tick wins the tail");
    }

    #[test]
    fn accepts_a_market_gap_after_an_empty_heal_without_refetching() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        // The provider has nothing in between: an empty interval, not a miss.
        rig.feed.set_range_result(Some(Vec::new()));
        rig.feed.push_tick(bar_at(last + 3 * HOUR, 100.0));
        rig.settle();
        assert_eq!(rig.feed.ranges().len(), 1);
        assert_eq!(bar_times(&rig), [last + 3 * HOUR]);

        // Another discontinuity inside the cooldown applies directly.
        rig.feed.push_tick(bar_at(last + 6 * HOUR, 100.0));
        rig.settle();
        assert_eq!(rig.feed.ranges().len(), 1);
        assert_eq!(bar_times(&rig), [last + 3 * HOUR, last + 6 * HOUR]);

        // Past the cooldown the next discontinuity heals again.
        rig.advance(HEAL_COOLDOWN_MS + 1.0);
        rig.feed.push_tick(bar_at(last + 9 * HOUR, 100.0));
        rig.settle();
        assert_eq!(rig.feed.ranges().len(), 2);
    }

    #[test]
    fn contiguous_live_bars_never_fetch() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        rig.feed.push_tick(bar_at(last, 101.0)); // the forming bar again
        rig.feed.push_tick(bar_at(last + HOUR, 100.0)); // exactly one interval on
        rig.settle();
        assert!(rig.feed.ranges().is_empty());
        assert_eq!(rig.raw().len(), 11);
        assert_eq!(rig.raw()[9].close, 101.0, "the forming bar was replaced");
    }

    #[test]
    fn a_heal_notifies_the_sessions_once_not_once_per_backfilled_bar() {
        let rig = gap_rig();
        let session = rig.attach_session("ind-1", "overlay=true");
        let last = T0 + 9 * HOUR;
        // 4 missed bars + the forming one come back; the live tick is buffered.
        rig.feed.set_range_result(Some(
            (0..6).map(|k| bar_at(last + k * HOUR, 100.0)).collect(),
        ));
        rig.feed.push_tick(bar_at(last + 5 * HOUR, 999.0));
        rig.settle();
        assert_eq!(session.notifies(), [None], "one re-run for the whole heal");
        assert!(bar_times(&rig).len() > 1, "the bar event stays per bar");

        // A normal contiguous tick still notifies at once.
        rig.feed.push_tick(bar_at(last + 5 * HOUR, 1000.0));
        assert_eq!(session.notifies(), [None, None]);
    }

    #[test]
    fn a_heal_whose_market_was_switched_away_drops_its_bars() {
        let rig = rig_with_prices()
            .live(true)
            .feed(|f| f.gate_ranges(true))
            .build();
        rig.ready().expect("ready");
        let last = T0 + 49 * HOUR;
        rig.feed.push_tick(bar_at(last + 3 * HOUR, 999.0)); // heal parks on the gate
        assert_eq!(rig.feed.ranges().len(), 1);
        rig.feed
            .set_range_result(Some(vec![bar_at(last + 3 * HOUR, 1.0)]));

        switch_to(&rig, to_symbol("BBB")).expect("switch");
        let events_before = bar_times(&rig).len();
        rig.feed.gate_ranges(false);
        rig.feed.release();
        rig.settle();
        // The old market's bars never reach the new array, and nothing is replayed.
        assert_eq!(bar_times(&rig).len(), events_before);
        assert_eq!(rig.raw().len(), 50);
        assert_eq!(rig.raw()[0].close, 200.0);
        assert!(!rig.chart.inner.state.borrow().healing);
    }

    #[test]
    fn a_heal_that_fails_transiently_still_applies_the_buffered_ticks() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        rig.feed.set_range_error(Some(DataError::other("blip")));
        rig.feed.push_tick(bar_at(last + 3 * HOUR, 7.0));
        rig.settle();
        assert_eq!(bar_times(&rig), [last + 3 * HOUR]);
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 0);
    }

    #[test]
    fn a_heal_refused_for_good_ends_the_markets_updates() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        rig.feed.set_range_error(Some(unavailable("AAA", "60")));
        assert_eq!(rig.feed.active_subscribers(), 1);
        rig.feed.push_tick(bar_at(last + 3 * HOUR, 7.0));
        rig.settle();
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 1);
        assert_eq!(rig.feed.active_subscribers(), 0, "nothing keeps polling");
    }

    #[test]
    fn a_gap_is_never_healed_on_offline_data() {
        let rig = Rig::builder().data(bars(10)).live(true).build();
        rig.ready().expect("ready");
        let last = T0 + 9 * HOUR;
        rig.feed.push_tick(bar_at(last + 5 * HOUR, 1.0));
        assert!(rig.feed.ranges().is_empty());
        assert_eq!(bar_times(&rig), [last + 5 * HOUR]);
        assert!(!rig.chart.can_heal());
    }

    #[test]
    fn offline_data_with_live_shows_the_given_bars_unchanged() {
        let data = bars(50);
        let rig = Rig::builder().data(data.clone()).live(true).build();
        rig.ready().expect("ready");
        rig.advance(60_000.0);
        assert!(bar_times(&rig).is_empty(), "no invented ticks or candles");
        assert_eq!(rig.raw(), data);
        assert!(rig.feed.ranges().is_empty());
    }

    #[test]
    fn the_page_visible_catch_up_heals_from_the_last_bar_once_per_cooldown() {
        let rig = gap_rig();
        let last = T0 + 9 * HOUR;
        rig.feed.set_range_result(Some(vec![bar_at(last, 5.0)]));
        rig.chart.on_page_visible();
        rig.settle();
        assert_eq!(rig.feed.ranges().len(), 1);
        assert_eq!(rig.feed.ranges()[0].from, Some(last));
        assert_eq!(rig.raw()[9].close, 5.0, "the stale close is corrected");
        // Inside the cooldown a second return does nothing.
        rig.chart.on_page_visible();
        assert_eq!(rig.feed.ranges().len(), 1);
        // A chart that is not live never armed the catch-up.
        let cold = Rig::builder().build();
        cold.ready().expect("ready");
        cold.chart.on_page_visible();
        assert!(cold.feed.ranges().is_empty());
    }

    #[test]
    fn a_tick_delivered_from_inside_subscribe_is_applied() {
        let tick = bar_at(T0 + 49 * HOUR, 1234.0);
        let rig = Rig::builder()
            .live(true)
            .feed(move |f| f.set_tick_on_subscribe(Some(tick)))
            .build();
        rig.ready().expect("ready");
        assert_eq!(rig.raw().last().map(|b| b.close), Some(1234.0));
        assert_eq!(bar_times(&rig).len(), 1);
    }

    #[test]
    fn a_tick_after_destroy_changes_nothing() {
        let rig = gap_rig();
        rig.chart.destroy();
        assert_eq!(rig.feed.active_subscribers(), 0);
        rig.feed.push_tick(bar_at(T0 + 20 * HOUR, 1.0));
        assert!(rig.renderer.updated_bars.borrow().is_empty());
        assert!(rig.renderer.destroyed.get());
        assert_eq!(rig.feed.count(|c| *c == FeedCall::Destroy), 1);
    }

    // ── viewport ──

    #[test]
    fn a_viewport_change_reruns_only_visible_range_scripts_after_the_debounce() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let vr = rig.attach_session("vr", "overlay=true visible");
        let plain = rig.attach_session("plain", "overlay=true");
        rig.renderer.fire_viewport(VisibleRange {
            from: 1_700_000_036_000,
            to: 1_700_000_144_000,
        });
        // Reported at once; the re-run waits for the debounce.
        assert_eq!(rig.log.count(ChartEventName::ViewportChanged), 1);
        assert!(vr.visible_ranges().is_empty());
        rig.advance(VIEWPORT_DEBOUNCE_MS + 70.0);
        assert_eq!(
            vr.visible_ranges(),
            [VisibleBarRange {
                left: 1_700_000_036_000.0,
                right: 1_700_000_144_000.0
            }]
        );
        assert!(plain.visible_ranges().is_empty());
        assert_eq!(
            rig.record("vr").borrow().pending_cause,
            Some(ScriptRunCause::Viewport)
        );
        assert_eq!(rig.record("plain").borrow().pending_cause, None);
    }

    #[test]
    fn a_burst_of_viewport_events_is_one_rerun() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let vr = rig.attach_session("vr", "overlay=true visible");
        for i in 0..8 {
            rig.renderer.fire_viewport(VisibleRange {
                from: T0 + i * 1000,
                to: T0 + 100_000,
            });
        }
        rig.advance(VIEWPORT_DEBOUNCE_MS + 70.0);
        assert_eq!(vr.visible_ranges().len(), 1, "8 events, 1 re-run");
        assert_eq!(rig.log.count(ChartEventName::ViewportChanged), 8);
        assert_eq!(vr.visible_ranges()[0].left, (T0 + 7000) as f64);
    }

    #[test]
    fn hidden_and_unmounted_indicators_are_not_poked_by_the_viewport() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let hidden = rig.attach_session("hidden", "overlay=true visible");
        let unmounted = rig.attach_session("unmounted", "overlay=true visible");
        rig.record("hidden").borrow_mut().hidden = true;
        rig.record("unmounted").borrow_mut().render_handle = None;
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(hidden.visible_ranges().is_empty());
        assert!(unmounted.visible_ranges().is_empty());
    }

    #[test]
    fn the_viewport_debounce_is_held_while_a_market_switch_is_in_flight() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let vr = rig.attach_session("vr", "overlay=true visible");
        rig.feed.gate_symbol("SLOW");
        let done = rig.chart.set_market(to_symbol("SLOW"));
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(
            vr.visible_ranges().is_empty(),
            "old sessions must not re-run mid-switch"
        );
        rig.feed.release();
        rig.block_on(done).expect("switch");
    }

    #[test]
    fn a_pending_viewport_timer_does_not_fire_after_destroy() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let vr = rig.attach_session("vr", "overlay=true visible");
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        rig.chart.destroy();
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(vr.visible_ranges().is_empty());
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    // ── theme, range, pan, resize ──

    #[test]
    fn set_theme_reskins_the_renderer_and_a_same_theme_call_is_a_no_op() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.chart.set_theme(light_theme());
        assert_eq!(*rig.renderer.themes.borrow(), [light_theme()]);
        let seen = rig.log.of(ChartEventName::ThemeChanged);
        assert_eq!(seen.len(), 1);
        assert!(matches!(&seen[0], ChartEvent::ThemeChanged(t) if *t == light_theme()));
        // Candle hues are shared across themes.
        assert_eq!(light_theme()["upColor"], dark_theme()["upColor"]);
        // Already active: no re-skin, no event (this breaks a host's echo loop).
        rig.chart.set_theme(light_theme());
        assert_eq!(rig.renderer.themes.borrow().len(), 1);
        assert_eq!(rig.log.count(ChartEventName::ThemeChanged), 1);
        rig.chart.set_theme(dark_theme());
        assert_eq!(rig.log.count(ChartEventName::ThemeChanged), 2);
    }

    #[test]
    fn the_renderers_theme_select_resolves_registered_ids_and_unknown_ones_fall_back_to_dark() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.renderer.fire_theme_select("light");
        assert_eq!(*rig.renderer.themes.borrow(), [light_theme()]);
        rig.renderer.fire_theme_select("nope");
        assert_eq!(rig.renderer.themes.borrow().len(), 2);
        assert_eq!(rig.renderer.themes.borrow()[1], dark_theme());
    }

    #[test]
    fn pan_by_prefers_the_renderers_own_pan_and_falls_back_to_a_range_shift() {
        let rig = Rig::builder()
            .renderer(|r| r.pan_by_supported.set(true))
            .build();
        rig.ready().expect("ready");
        let before = rig.renderer.visible_range_calls.borrow().len();
        rig.chart.pan_by(0.2);
        rig.chart.pan_by(-0.2);
        assert_eq!(*rig.renderer.pan_calls.borrow(), [0.2, -0.2]);
        assert_eq!(rig.renderer.visible_range_calls.borrow().len(), before);

        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let before = rig.renderer.visible_range_calls.borrow().len();
        rig.chart.pan_by(0.5); // nothing to shift: no throw
        assert_eq!(rig.renderer.visible_range_calls.borrow().len(), before);
        rig.renderer.visible_range.set(Some(VisibleRange {
            from: 10_000,
            to: 20_000,
        }));
        rig.chart.pan_by(0.5); // span 10000 x 0.5: shift +5000
        rig.chart.pan_by(-0.5);
        let calls = rig.renderer.visible_range_calls.borrow().clone();
        assert_eq!(
            calls[before].0,
            VisibleRange {
                from: 15_000,
                to: 25_000
            }
        );
        assert_eq!(
            calls[before + 1].0,
            VisibleRange {
                from: 5_000,
                to: 15_000
            }
        );
    }

    #[test]
    fn a_preset_frames_the_loaded_bars_with_the_right_margin() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.chart.set_visible_range_preset(VisibleRangePreset::All);
        let calls = rig.renderer.visible_range_calls.borrow().clone();
        let last = calls.last().expect("framed");
        assert_eq!(last.0.from, T0);
        assert_eq!(last.0.to, T0 + 49 * HOUR);
        assert!(last.1.right_margin);
        // No bars, no frame.
        let empty = Rig::builder()
            .data(Vec::new())
            .feed(|f| f.set_depth("AAA", 0))
            .build();
        empty.ready().expect("ready");
        empty
            .chart
            .set_visible_range_preset(VisibleRangePreset::All);
        assert!(empty.renderer.visible_range_calls.borrow().is_empty());
    }

    // ── price style: bar transform ──

    #[test]
    fn switching_to_heikin_ashi_rebuilds_the_view_and_reexecutes_indicators_on_it() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        let session = rig.attach_session("ind-1", "overlay=true");
        let raw = bars(50);
        let ha = heikin_ashi_full(&raw);
        assert_eq!(*rig.renderer.bars.borrow(), raw);

        rig.renderer.fire_price_style("heikinashi");
        // The renderer got the DERIVED series without a viewport jump...
        assert_eq!(*rig.renderer.bars.borrow(), ha);
        assert_eq!(
            rig.renderer
                .last_bars_call()
                .map(|c| (c.n, c.preserve_view)),
            Some((50, true))
        );
        // ...and the indicator was torn down for a re-run on the new view.
        assert_eq!(session.stops(), 1);
        let record = rig.record("ind-1");
        assert!(record.borrow().session.is_some());
        assert_eq!(rig.engine.sessions.borrow().len(), 2);
        assert!(record.borrow().pending_structural);
        assert_eq!(record.borrow().pending_cause, Some(ScriptRunCause::Market));
        assert!(record.borrow().loading);
        assert_eq!(
            rig.chart.market().chart_style.as_deref(),
            Some("heikinashi")
        );
        // The raw series underneath is untouched.
        assert_eq!(rig.raw(), raw);

        // Back: the raw view again.
        rig.renderer.fire_price_style("candles");
        assert_eq!(*rig.renderer.bars.borrow(), raw);
        assert_eq!(rig.chart.market().chart_style.as_deref(), Some("candles"));
    }

    #[test]
    fn a_chart_constructed_in_heikin_ashi_loads_the_derived_view_from_the_start() {
        let rig = Rig::builder().price_style("heikinashi").build();
        rig.ready().expect("ready");
        assert_eq!(*rig.renderer.bars.borrow(), heikin_ashi_full(&bars(50)));
        assert_eq!(rig.raw(), bars(50));
    }

    #[test]
    fn live_ticks_derive_the_forming_view_bar_incrementally_and_the_bar_event_stays_raw() {
        let rig = Rig::builder()
            .live(true)
            .feed(|f| f.default_depth.set(10))
            .build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style("heikinashi");
        let raw = bars(10);
        let last = raw[9].time;
        let corrected = bar_at(last, 105.0);
        let appended = bar_at(last + HOUR, 106.0);
        rig.feed.push_tick(corrected);
        rig.feed.push_tick(appended);

        let mut stream = raw[..9].to_vec();
        stream.push(corrected);
        stream.push(appended);
        let expected = heikin_ashi_full(&stream);
        let updated = rig.renderer.updated_bars.borrow().clone();
        assert_eq!(updated[updated.len() - 2..], [expected[9], expected[10]]);
        // The bar event carries the RAW bars: the data plane never sees synthetic values.
        let events: Vec<Ohlcv> = rig
            .log
            .of(ChartEventName::Bar)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::Bar(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(events[events.len() - 2..], [corrected, appended]);
        assert_eq!(rig.view(), expected);
        assert_eq!(rig.raw(), stream);
    }

    #[test]
    fn the_bar_cause_tells_a_forming_tick_from_a_new_bar() {
        let rig = Rig::builder()
            .live(true)
            .feed(|f| f.default_depth.set(10))
            .build();
        rig.ready().expect("ready");
        let session = rig.attach_session("ind-1", "overlay=true");
        let last = T0 + 9 * HOUR;
        rig.feed.push_tick(bar_at(last, 1.0));
        assert_eq!(
            rig.record("ind-1").borrow().pending_cause,
            Some(ScriptRunCause::Tick)
        );
        rig.feed.push_tick(bar_at(last + HOUR, 1.0));
        assert_eq!(
            rig.record("ind-1").borrow().pending_cause,
            Some(ScriptRunCause::Bar)
        );
        assert_eq!(session.notifies(), [None, None]);
    }

    #[test]
    fn fetch_series_decides_the_transform_by_the_extended_ticker() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style("heikinashi");
        let raw = bars(50);
        let ha = heikin_ashi_full(&raw);
        let range = BarRange {
            limit: Some(50),
            ..BarRange::default()
        };
        for (symbol, expected) in [
            ("TEST", &raw),
            ("OTHER", &raw),
            ("TEST;heikinashi", &ha),
            ("OTHER;heikinashi", &ha),
            ("TEST;standard", &raw),
        ] {
            let fetched = rig.chart.fetch_series(symbol, "60", &range);
            assert_eq!(&rig.block_on(fetched).expect(symbol), expected, "{symbol}");
        }
        // A rangeless feed answers through `load` with the limit as the depth.
        let rangeless = Rig::builder().ranged(false).build();
        rangeless.ready().expect("ready");
        let fetched = rangeless.chart.fetch_series("TEST", "60", &range);
        assert_eq!(rangeless.block_on(fetched).expect("loads"), raw);
        assert_eq!(rangeless.feed.loads().last().and_then(|c| c.bars), Some(50));
    }

    #[test]
    fn fetch_series_errors_reach_the_caller_and_never_the_chart() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.feed.set_range_error(Some(unavailable("OTHER", "1")));
        let fetched = rig.chart.fetch_series("OTHER", "1", &BarRange::default());
        assert!(matches!(
            rig.block_on(fetched),
            Err(DataError::Unavailable(_))
        ));
        // The engines' gateway never reports the chart's own market as unavailable.
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 0);
    }

    #[derive(Debug)]
    struct Doubled;

    impl BarTransform for Doubled {
        fn full(&self, raw: &[Ohlcv]) -> Vec<Ohlcv> {
            raw.iter().map(|b| self.next(b, None)).collect()
        }

        fn next(&self, raw: &Ohlcv, _prev: Option<&Ohlcv>) -> Ohlcv {
            Ohlcv::new(
                raw.time,
                raw.open * 2.0,
                raw.high * 2.0,
                raw.low * 2.0,
                raw.close * 2.0,
                raw.volume,
            )
        }
    }

    #[test]
    fn a_registered_bar_transform_type_rides_the_same_path_as_heikin_ashi() {
        let rig = Rig::builder()
            .registries(|r| {
                r.chart_types.borrow_mut().register(ChartTypeDefinition {
                    bar_transform: Some(Arc::new(Doubled)),
                    ..ChartTypeDefinition::new("doubled")
                });
            })
            .build();
        rig.ready().expect("ready");
        let raw = bars(50);
        assert_eq!(*rig.renderer.bars.borrow(), raw);
        rig.renderer.fire_price_style("doubled");
        assert_eq!(*rig.renderer.bars.borrow(), Doubled.full(&raw));
        assert_eq!(
            rig.renderer
                .last_bars_call()
                .map(|c| (c.n, c.preserve_view)),
            Some((50, true))
        );
        rig.renderer.fire_price_style("candles");
        assert_eq!(*rig.renderer.bars.borrow(), raw);
        // The same style again is a no-op: no second rebuild.
        let n = rig.renderer.set_bars_calls.borrow().len();
        rig.renderer.fire_price_style("candles");
        assert_eq!(rig.renderer.set_bars_calls.borrow().len(), n);
    }

    #[test]
    fn the_price_style_will_change_notice_reaches_the_bus() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style_will_change("candles", "line");
        let seen = rig.log.of(ChartEventName::PriceStyleChange);
        assert!(matches!(
            &seen[..],
            [ChartEvent::PriceStyleChange { from, to }] if from == "candles" && to == "line"
        ));
    }

    // ── price style: chart-type data engines ──

    thread_local! {
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        static HOSTS: RefCell<Vec<Arc<dyn crate::chart_types::SeriesDataEngineHost>>> =
            const { RefCell::new(Vec::new()) };
        static SETTINGS: RefCell<Vec<serde_json::Map<String, Value>>> =
            const { RefCell::new(Vec::new()) };
    }

    fn calls_log() -> Vec<String> {
        CALLS.with(|c| c.borrow().clone())
    }

    fn host(n: usize) -> Arc<dyn crate::chart_types::SeriesDataEngineHost> {
        HOSTS.with(|h| Arc::clone(&h.borrow()[n]))
    }

    fn host_count() -> usize {
        HOSTS.with(|h| h.borrow().len())
    }

    /// A data engine that logs its lifecycle (on the test's thread) and pushes what
    /// it was configured to push.
    struct Probe {
        start_push: Option<Value>,
        push_symbol: bool,
        resume_push: Option<Value>,
    }

    impl SeriesDataEngine for Probe {
        fn start(&mut self, host: Arc<dyn crate::chart_types::SeriesDataEngineHost>) {
            CALLS.with(|c| c.borrow_mut().push(format!("start:{}", host.bars().len())));
            if let Some(push) = &self.start_push {
                host.push_data(push.clone());
            }
            if self.push_symbol {
                host.push_data(json!({ "cells": host.symbol() }));
            }
            HOSTS.with(|h| h.borrow_mut().push(host));
        }

        fn suspend(&mut self) {
            CALLS.with(|c| c.borrow_mut().push("suspend".into()));
        }

        fn resume(&mut self) {
            CALLS.with(|c| c.borrow_mut().push("resume".into()));
            if let Some(push) = &self.resume_push {
                let host = HOSTS.with(|h| Arc::clone(&h.borrow()[0]));
                host.push_data(push.clone());
            }
        }

        fn stop(&mut self) {
            CALLS.with(|c| c.borrow_mut().push("stop".into()));
        }

        fn on_viewport(&mut self, range: VisibleRange) {
            CALLS.with(|c| c.borrow_mut().push(format!("viewport:{}", range.from)));
        }

        fn on_bars(&mut self) {
            CALLS.with(|c| c.borrow_mut().push("bars".into()));
        }

        fn on_settings(&mut self, values: &serde_json::Map<String, Value>) {
            SETTINGS.with(|s| s.borrow_mut().push(values.clone()));
        }
    }

    fn probe_type(
        id: &str,
        start_push: Option<Value>,
        push_symbol: bool,
        resume_push: Option<Value>,
    ) -> ChartTypeDefinition {
        ChartTypeDefinition {
            data_engine: Some(Arc::new(move || {
                Box::new(Probe {
                    start_push: start_push.clone(),
                    push_symbol,
                    resume_push: resume_push.clone(),
                }) as Box<dyn SeriesDataEngine>
            })),
            ..ChartTypeDefinition::new(id)
        }
    }

    fn with_probe(rig: RigBuilder, def: ChartTypeDefinition) -> RigBuilder {
        rig.registries(|r| r.chart_types.borrow_mut().register(def))
    }

    #[test]
    fn a_data_engine_type_starts_on_entry_pushes_suspends_resumes_and_stops_at_destroy() {
        let rig = with_probe(
            Rig::builder(),
            probe_type("doubled", Some(json!(["payload"])), false, None),
        )
        .build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style("doubled");
        rig.settle();
        assert_eq!(
            calls_log(),
            ["start:50"],
            "created lazily, started after ready"
        );
        assert_eq!(rig.renderer.pushes_to("doubled"), [json!(["payload"])]);
        let h = host(0);
        assert_eq!(
            h.bars().len(),
            50,
            "the host serves the chart's current view bars"
        );
        assert!(!h.live());
        h.push_pending(&[(1, 2)]);
        assert_eq!(rig.renderer.pushes_to("doubled-pending"), [json!([[1, 2]])]);

        // Pan/zoom pokes the ACTIVE engine (debounced).
        rig.renderer
            .fire_viewport(VisibleRange { from: 123, to: 456 });
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(calls_log().contains(&"viewport:123".to_string()));

        // Leaving the style suspends, never stops; re-entry is a cheap resume.
        rig.renderer.fire_price_style("candles");
        assert_eq!(calls_log().last().map(String::as_str), Some("suspend"));
        rig.renderer.fire_price_style("doubled");
        rig.settle();
        assert_eq!(calls_log().last().map(String::as_str), Some("resume"));
        assert_eq!(host_count(), 1, "no second engine");

        rig.chart.destroy();
        assert_eq!(calls_log().last().map(String::as_str), Some("stop"));
    }

    #[test]
    fn the_engine_host_hands_over_the_charts_data_control() {
        use crate::controls::data_control::DataControl;
        use crate::data::BarStore;
        use crate::data::multi_provider_feed::MultiProviderFeed;
        let rig = with_probe(
            Rig::builder().data_control(|platform| {
                let registry = MultiProviderFeed::new(
                    Rc::new(RefCell::new(BarStore::new())),
                    platform.handle(),
                );
                DataControl::new(Some(registry), platform.handle())
            }),
            probe_type("doubled", None, false, None),
        )
        .build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style("doubled");
        rig.settle();
        assert!(host(0).data().has_registry(), "the chart's own control");
    }

    #[test]
    fn a_suspended_engine_pushes_nothing_until_it_resumes_and_a_stopped_one_stays_silent() {
        let rig = with_probe(
            Rig::builder(),
            probe_type("doubled", None, false, Some(json!(["resumed"]))),
        )
        .build();
        rig.ready().expect("ready");
        rig.renderer.fire_price_style("doubled");
        rig.settle();
        rig.renderer.fire_price_style("candles");

        rig.renderer.native_pushes.borrow_mut().clear();
        // A fetch resolving after the style was left.
        host(0).push_data(json!(["late"]));
        host(0).push_pending(&[(1, 2)]);
        assert!(rig.renderer.native_pushes.borrow().is_empty());

        rig.renderer.fire_price_style("doubled");
        rig.settle();
        // The resume's re-push lands.
        assert_eq!(rig.renderer.pushes_to("doubled"), [json!(["resumed"])]);

        rig.chart.destroy();
        rig.renderer.native_pushes.borrow_mut().clear();
        host(0).push_data(json!(["after-destroy"]));
        assert!(rig.renderer.native_pushes.borrow().is_empty());
    }

    #[test]
    fn a_chart_constructed_in_an_engine_style_starts_the_engine_only_after_the_first_load() {
        let rig = with_probe(
            Rig::builder()
                .price_style("doubled")
                .feed(|f| f.gate_symbol("AAA")),
            probe_type("doubled", None, false, None),
        )
        .build();
        rig.settle();
        // The first load is held: the engine must wait behind it, not start on no data.
        assert!(calls_log().is_empty());
        rig.feed.release();
        rig.ready().expect("ready");
        rig.settle();
        assert_eq!(
            calls_log(),
            ["start:50"],
            "the engine saw a loaded market at start"
        );
    }

    #[test]
    fn renderer_settings_edits_reach_the_active_type_engine_and_are_replayed_into_new_ones() {
        let rig = with_probe(
            Rig::builder().price_style("doubled"),
            probe_type("doubled", None, false, None),
        )
        .build();
        rig.ready().expect("ready");
        rig.settle();
        let mut values = serde_json::Map::new();
        values.insert("levels".into(), json!(12));
        rig.renderer.fire_chart_type_settings("doubled", &values);
        assert_eq!(SETTINGS.with(|s| s.borrow().clone()), [values.clone()]);
        // Only the matching engine hears it.
        rig.renderer
            .fire_chart_type_settings("other-type", &serde_json::Map::new());
        assert_eq!(SETTINGS.with(|s| s.borrow().len()), 1);

        // A market switch recreates the engine: the cached values reach it before start.
        SETTINGS.with(|s| s.borrow_mut().clear());
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert_eq!(SETTINGS.with(|s| s.borrow().clone()), [values]);
    }

    #[test]
    fn the_active_engine_is_poked_when_the_bars_change() {
        let rig = with_probe(
            Rig::builder().live(true).feed(|f| f.default_depth.set(10)),
            probe_type("doubled", None, false, None),
        )
        .price_style("doubled")
        .build();
        rig.ready().expect("ready");
        rig.settle();
        rig.feed.push_tick(bar_at(T0 + 9 * HOUR, 1.0));
        assert!(calls_log().contains(&"bars".to_string()));
    }

    // ── set_market ──

    #[test]
    fn a_switch_reloads_the_bars_reframes_and_emits_market_changed_with_prev() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let r = switch_to(
            &rig,
            MarketSwitch {
                symbol: Some("BBB".into()),
                timeframe: Some("240".into()),
                ..MarketSwitch::default()
            },
        );
        assert_eq!(r, Ok(()));
        assert_eq!(close0(&rig), Some(200.0));
        // A fresh series re-frames the view.
        assert!(
            !rig.renderer
                .last_bars_call()
                .expect("painted")
                .preserve_view
        );
        assert_eq!(
            market_changed(&rig),
            [(
                "BBB".to_string(),
                "240".to_string(),
                MarketIdentity {
                    symbol: "AAA".into(),
                    timeframe: "60".into()
                }
            )]
        );
        let load = rig.feed.loads().pop().expect("a load");
        assert_eq!(
            (load.symbol.as_deref(), load.timeframe.as_deref()),
            (Some("BBB"), Some("240"))
        );
    }

    #[test]
    fn the_market_snapshot_reports_the_requested_identity_the_moment_a_switch_is_called() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let snap = rig.chart.market_snapshot();
        assert_eq!(
            (
                snap.symbol.as_deref(),
                snap.timeframe.as_deref(),
                snap.offline
            ),
            (Some("AAA"), Some("60"), false)
        );
        let done = rig.chart.set_market(MarketSwitch {
            symbol: Some("BBB".into()),
            timeframe: Some("240".into()),
            ..MarketSwitch::default()
        });
        // Before the load lands: the persist-on-close read must capture the INTENT.
        let snap = rig.chart.market_snapshot();
        assert_eq!(
            (snap.symbol.as_deref(), snap.timeframe.as_deref()),
            (Some("BBB"), Some("240"))
        );
        rig.block_on(done).expect("switch");
        // A copy: mutating it changes nothing.
        let mut snap = rig.chart.market_snapshot();
        snap.symbol = Some("ZZZ".into());
        assert_eq!(rig.chart.market_snapshot().symbol.as_deref(), Some("BBB"));
        // The venue a symbol pins is part of the snapshot.
        switch_to(&rig, to_symbol("coinbase:BTC-USD")).expect("switch");
        assert_eq!(
            rig.chart.market_snapshot().provider.as_deref(),
            Some("coinbase")
        );
    }

    #[test]
    fn a_session_switch_reloads_like_a_timeframe_change_and_the_flag_rides_the_load() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Extended),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        let load = rig.feed.loads().pop().expect("a load");
        assert_eq!(load.symbol.as_deref(), Some("AAA"));
        assert_eq!(load.session, Some(MarketSession::Extended));
        assert_eq!(
            rig.chart.market_snapshot().session,
            Some(MarketSession::Extended)
        );
        assert_eq!(
            market_changed(&rig).len(),
            1,
            "an identity change: the chrome re-syncs"
        );
        // The same session again: no reload.
        let loads = rig.feed.loads().len();
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Extended),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.feed.loads().len(), loads);
    }

    #[test]
    fn a_session_only_flip_carries_the_zoom_and_an_explicit_frame_wins() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let window = VisibleRange {
            from: 1_700_010_000_000,
            to: 1_700_020_000_000,
        };
        rig.renderer.visible_range.set(Some(window));
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Extended),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        let framed: Vec<_> = rig
            .renderer
            .visible_range_calls
            .borrow()
            .iter()
            .map(|c| c.0)
            .collect();
        assert!(
            framed.contains(&window),
            "the same window re-framed the reload"
        );

        rig.renderer.visible_range_calls.borrow_mut().clear();
        let asked = VisibleRange {
            from: T0,
            to: T0 + 5_000_000,
        };
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Regular),
                visible_range: Some(InitialRange::Range(asked)),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        let framed: Vec<_> = rig
            .renderer
            .visible_range_calls
            .borrow()
            .iter()
            .map(|c| c.0)
            .collect();
        assert!(framed.contains(&asked));
        assert!(!framed.contains(&window));
    }

    #[test]
    fn the_carried_view_keeps_the_bar_count_not_the_wall_clock() {
        // Extended = hourly bars; regular = every OTHER hour (half density), same right
        // edge: the RTH/ETH shape.
        let end = 1_700_216_000_000_i64;
        fn series(end: i64, step: i64) -> Vec<Ohlcv> {
            (0..60)
                .map(|i| Ohlcv::new(end - (59 - i) * step, 1.0, 2.0, 0.0, 1.0, Some(1.0)))
                .collect()
        }
        let rig = Rig::builder()
            .session(MarketSession::Extended)
            .feed(move |f| {
                f.set_session_series(MarketSession::Extended, series(end, 3_600_000));
                f.set_session_series(MarketSession::Regular, series(end, 7_200_000));
            })
            .build();
        rig.ready().expect("ready");
        // The user looks at the last 10 hourly bars.
        rig.renderer.visible_range.set(Some(VisibleRange {
            from: end - 9 * 3_600_000 - 1,
            to: end + 1,
        }));
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Regular),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        // Refined frame: 10 bars of the NEW series, twice the wall clock, same zoom.
        let last = rig
            .renderer
            .visible_range_calls
            .borrow()
            .last()
            .copied()
            .expect("framed");
        assert_eq!(
            last.0,
            VisibleRange {
                from: end - 9 * 7_200_000,
                to: end + 1
            }
        );
    }

    #[test]
    fn a_session_only_flip_deepens_the_reload_to_reach_the_carried_window_but_never_mega_loads() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let now = rig.platform.now_ms() as i64;
        // The user panned ~900 hourly bars into the past: 500 bars cannot reach it.
        rig.renderer.visible_range.set(Some(VisibleRange {
            from: now - 900 * HOUR,
            to: now - 880 * HOUR,
        }));
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Extended),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        let deep = rig.feed.loads().last().and_then(|c| c.bars).expect("bars");
        assert!(
            (900..=5000).contains(&deep),
            "reaches the window, never a mega-load: {deep}"
        );

        // A NEARBY window needs no deepening, and depth never shrinks.
        rig.renderer.visible_range.set(Some(VisibleRange {
            from: now - 100 * HOUR,
            to: now - 90 * HOUR,
        }));
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Regular),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.feed.loads().last().and_then(|c| c.bars), Some(deep));
    }

    #[test]
    fn a_symbol_switch_keeps_todays_reframe_and_a_requested_window_frames_the_first_paint() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let stale = VisibleRange {
            from: 1_700_010_000_000,
            to: 1_700_020_000_000,
        };
        rig.renderer.visible_range.set(Some(stale));
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        // No stale window carried to a new clock.
        assert!(rig.renderer.visible_range_calls.borrow().is_empty());

        let range = VisibleRange {
            from: T0,
            to: T0 + 36_000_000,
        };
        switch_to(
            &rig,
            MarketSwitch {
                symbol: Some("CCC".into()),
                visible_range: Some(InitialRange::Range(range)),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        let framed: Vec<_> = rig
            .renderer
            .visible_range_calls
            .borrow()
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(framed, [range]);
    }

    #[test]
    fn a_same_market_call_is_a_no_op_except_for_a_framing_request() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let loads = rig.feed.loads().len();
        switch_to(
            &rig,
            MarketSwitch {
                symbol: Some("AAA".into()),
                timeframe: Some("60".into()),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        switch_to(&rig, MarketSwitch::default()).expect("switch");
        assert_eq!(rig.feed.loads().len(), loads);
        assert!(market_changed(&rig).is_empty());
        // Only the framing is honored.
        let range = VisibleRange { from: 5, to: 9 };
        switch_to(
            &rig,
            MarketSwitch {
                visible_range: Some(InitialRange::Range(range)),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.renderer.visible_range_calls.borrow()[0].0, range);
        switch_to(
            &rig,
            MarketSwitch {
                visible_range: Some(InitialRange::Preset(VisibleRangePreset::All)),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.renderer.visible_range_calls.borrow().len(), 2);
        assert_eq!(rig.feed.loads().len(), loads);
    }

    #[test]
    fn an_identity_switch_blanks_the_mounted_indicator_visuals_for_the_gap() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.attach_session("ind-1", "overlay=true");
        rig.feed.gate_symbol("SLOW");
        let done = rig.chart.set_market(to_symbol("SLOW"));
        // Synchronously: nothing stale outlives the old market.
        let mounted = rig.renderer.mounted_models.borrow().clone();
        let blank = mounted.last().expect("a blanked remount");
        assert_eq!(blank.id, "ind-1");
        assert!(
            blank
                .series
                .iter()
                .all(|s| s.points().is_some_and(<[_]>::is_empty))
        );
        assert_eq!(blank.anchor_time, None);
        let record = rig.record("ind-1");
        assert!(
            record.borrow().pending_structural,
            "the next model remounts over the blank"
        );
        assert!(record.borrow().loading);
        assert_eq!(
            rig.renderer.statuses.borrow().last(),
            Some(&(
                "ind-1".to_string(),
                crate::ports::chart_renderer::IndicatorStatus::Loading
            ))
        );
        assert!(rig.renderer.bars.borrow().is_empty());
        assert!(rig.renderer.loading.get());
        rig.feed.release();
        rig.block_on(done).expect("switch");
    }

    #[test]
    fn a_switch_reexecutes_sessions_over_the_new_market() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let session = rig.attach_session("ind-1", "overlay=true");
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert_eq!(session.stops(), 1);
        let record = rig.record("ind-1");
        // Torn down, and the execute followed: a fresh session over the new market.
        assert!(record.borrow().session.is_some());
        assert_eq!(rig.engine.sessions.borrow().len(), 2);
        assert_eq!(record.borrow().pending_cause, Some(ScriptRunCause::Market));
        assert!(record.borrow().pending_structural);
    }

    #[test]
    fn sessions_are_not_notified_while_a_switch_is_in_flight() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let session = rig.attach_session("ind-1", "overlay=true");
        rig.feed.gate_symbol("SLOW");
        let done = rig.chart.set_market(to_symbol("SLOW"));
        rig.chart.notify_sessions_bars(None);
        rig.chart
            .notify_sessions_bars(Some(BarsChangeReason::Backfill));
        assert!(
            session.notifies().is_empty(),
            "old sessions must not run over the incoming bars"
        );
        rig.feed.release();
        rig.block_on(done).expect("switch");
    }

    #[test]
    fn a_depth_only_reload_fetches_older_bars_without_announcing_a_market_change() {
        let rig = Rig::builder()
            .bars(100)
            .feed(|f| f.set_depth("AAA", 900))
            .build();
        rig.ready().expect("ready");
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(800),
                ..MarketSwitch::default()
            },
        )
        .expect("switch"); // resolves on what is painted...
        let complete = rig.chart.history_complete(); // ...the depth streams in behind it
        rig.block_on(complete);
        assert_eq!(rig.renderer.bars.borrow().len(), 800);
        assert!(market_changed(&rig).is_empty());
        assert_eq!(
            rig.log.count(ChartEventName::LoadStart),
            0,
            "never re-raises the affordance (the first load's start predates the listener)"
        );
    }

    #[test]
    fn a_depth_only_change_keeps_the_bars_painted_and_never_repushes_the_series() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.renderer.clear_timeline();
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(80),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        rig.loaded();
        // The feed has nothing older than its 50, so there is nothing to prepend: no
        // series push at all (a reload would re-frame the viewport).
        assert!(rig.renderer.timeline.borrow().is_empty());
        assert_eq!(rig.renderer.bars.borrow().len(), 50);
        assert!(!rig.renderer.loading.get());
        assert_eq!(
            completions(&rig).last().map(|c| c.0),
            Some(HistoryReason::Genesis)
        );
    }

    #[test]
    fn growing_the_depth_prepends_older_bars_and_keeps_the_view() {
        let rig = Rig::builder()
            .bars(50)
            .feed(|f| f.set_depth("AAA", 400))
            .build();
        rig.loaded();
        let first_head = rig.renderer.bars.borrow()[0].time;
        let newest = rig.renderer.bars.borrow().last().expect("bars").time;
        rig.renderer.set_bars_calls.borrow_mut().clear();
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(200),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        rig.loaded();
        let bars = rig.renderer.bars.borrow().clone();
        assert_eq!(bars.len(), 200, "the extra depth arrived");
        assert!(bars[0].time < first_head, "as OLDER bars, prepended");
        assert_eq!(
            bars.last().map(|b| b.time),
            Some(newest),
            "the newest end is untouched"
        );
        // EVERY push of the extension preserves the viewport.
        assert!(!rig.renderer.set_bars_calls.borrow().is_empty());
        assert!(
            rig.renderer
                .set_bars_calls
                .borrow()
                .iter()
                .all(|c| c.preserve_view)
        );
        assert!(!rig.renderer.loading.get());
    }

    #[test]
    fn shrinking_the_depth_trims_in_place_keeping_the_newest_bars_and_the_view() {
        let rig = Rig::builder()
            .bars(300)
            .feed(|f| f.set_depth("AAA", 400))
            .build();
        rig.loaded();
        let newest = rig.renderer.bars.borrow().last().expect("bars").time;
        assert_eq!(rig.renderer.bars.borrow().len(), 300);
        rig.renderer.set_bars_calls.borrow_mut().clear();
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(100),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        rig.settle();
        assert_eq!(rig.renderer.bars.borrow().len(), 100);
        assert_eq!(
            rig.renderer.bars.borrow().last().map(|b| b.time),
            Some(newest)
        );
        assert_eq!(calls(&rig), [(100, true)]);
        assert!(!rig.renderer.loading.get());
        assert_eq!(rig.raw().len(), 100);
    }

    #[test]
    fn a_depth_change_on_offline_data_reloads_instead_of_extending() {
        let rig = Rig::builder().data(bars(50)).build();
        rig.ready().expect("ready");
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(500),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        // No source to extend from: the reload is the only way (and re-frames).
        assert!(
            !rig.renderer
                .last_bars_call()
                .expect("painted")
                .preserve_view
        );
        assert!(market_changed(&rig).is_empty());
    }

    #[test]
    fn the_last_of_two_rapid_switches_wins_and_the_superseded_one_resolves_silently() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.feed.gate_symbol("SLOW");
        let slow = rig.chart.set_market(to_symbol("SLOW")); // the load parks on the gate
        let fast = rig.chart.set_market(to_symbol("BBB")); // supersedes it
        assert_eq!(rig.block_on(fast), Ok(()));
        // Must resolve promptly (raced against supersession), not hang.
        assert_eq!(rig.block_on(slow), Ok(()));
        assert_eq!(close0(&rig), Some(200.0));
        // The stale SLOW load finally lands... and is dropped, never painted.
        rig.feed.release();
        rig.settle();
        assert_eq!(close0(&rig), Some(200.0));
        let symbols: Vec<_> = market_changed(&rig).into_iter().map(|e| e.0).collect();
        assert_eq!(symbols, ["BBB"], "one switch, one event");
        assert!(!rig.chart.inner.state.borrow().switching_market);
    }

    #[test]
    fn a_switch_to_offline_data_and_back_to_a_provider() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let offline = flat_bars(30, 777.0);
        switch_to(
            &rig,
            MarketSwitch {
                data: Some(offline),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.renderer.bars.borrow().len(), 30);
        assert_eq!(close0(&rig), Some(777.0));
        assert!(rig.chart.market_snapshot().offline);
        // A symbol switch drops the offline dataset: back to the provider path.
        switch_to(&rig, to_symbol("CCC")).expect("switch");
        assert_eq!(close0(&rig), Some(300.0));
        assert!(!rig.chart.market_snapshot().offline);
    }

    #[test]
    fn a_switch_mid_backfill_abandons_the_old_backfill_loop() {
        let rig = rig_with_prices()
            .bars(12_000)
            .feed(|f| {
                f.set_depth("AAA", 15_000);
                f.gate_ranges(true);
            })
            .build();
        rig.ready().expect("ready"); // the first chunk painted; the backfill parked
        let old_complete = rig.chart.history_complete();
        switch_to(
            &rig,
            MarketSwitch {
                symbol: Some("BBB".into()),
                bars: Some(50),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        // The superseded load's future resolves rather than hanging.
        rig.block_on(old_complete);
        assert_eq!(close0(&rig), Some(200.0));
        let progress_before = rig.log.count(ChartEventName::HistoryProgress);

        // The old backfill chunk finally lands...
        rig.feed.gate_ranges(false);
        rig.feed.release();
        rig.settle();
        // ...but the abandoned loop emitted nothing and the new market stayed put.
        assert_eq!(
            rig.log.count(ChartEventName::HistoryProgress),
            progress_before
        );
        assert_eq!(close0(&rig), Some(200.0));
        assert_eq!(rig.renderer.bars.borrow().len(), 50);
    }

    #[test]
    fn destroy_mid_switch_releases_the_awaiter() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.feed.gate_symbol("SLOW");
        let pending = rig.chart.set_market(to_symbol("SLOW"));
        rig.chart.destroy();
        // Resolves (silently) instead of hanging on the parked load.
        assert_eq!(rig.block_on(pending), Ok(()));
        // The load landing after destroy paints nothing.
        let painted = rig.renderer.set_bars_calls.borrow().len();
        rig.feed.release();
        rig.settle();
        assert_eq!(rig.renderer.set_bars_calls.borrow().len(), painted);
        assert!(market_changed(&rig).is_empty());
    }

    #[test]
    fn the_history_complete_future_is_rearmed_per_load() {
        let rig = rig_with_prices().build();
        let first = rig.chart.history_complete();
        rig.ready().expect("ready");
        rig.block_on(first);
        let reasons: Vec<_> = completions(&rig).iter().map(|c| c.0).collect();
        assert_eq!(reasons, [HistoryReason::Depth]);

        switch_to(&rig, to_symbol("BBB")).expect("switch");
        let second = rig.chart.history_complete(); // the NEW load's future
        rig.block_on(second);
        let reasons: Vec<_> = completions(&rig).iter().map(|c| c.0).collect();
        assert_eq!(reasons, [HistoryReason::Depth, HistoryReason::Depth]);
        // Awaiting it any number of times is fine.
        let again = rig.chart.history_complete();
        rig.block_on(again);
    }

    #[test]
    fn a_switch_retargets_the_live_subscription() {
        let rig = rig_with_prices().live(true).build();
        rig.ready().expect("ready");
        let (subs, unsubs) = rig.feed.subscriptions();
        assert_eq!(subs, HashMap::from([("AAA".to_string(), 1)]));
        assert!(unsubs.is_empty());
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        let (subs, unsubs) = rig.feed.subscriptions();
        assert_eq!(
            unsubs,
            HashMap::from([("AAA".to_string(), 1)]),
            "the old stream closed"
        );
        assert_eq!(
            subs,
            HashMap::from([("AAA".to_string(), 1), ("BBB".to_string(), 1)]),
            "the new stream opened"
        );
        assert_eq!(rig.feed.active_subscribers(), 1);
    }

    #[test]
    fn the_depth_only_path_restarts_live_without_a_market_change() {
        let rig = Rig::builder()
            .live(true)
            .bars(20)
            .feed(|f| f.set_depth("AAA", 100))
            .build();
        rig.ready().expect("ready");
        switch_to(
            &rig,
            MarketSwitch {
                bars: Some(10),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(
            rig.feed.active_subscribers(),
            1,
            "live is back after the trim"
        );
        assert!(market_changed(&rig).is_empty());
    }

    // ── load states ──

    #[test]
    fn an_identity_switch_blanks_the_chart_immediately_and_raises_the_affordance() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.feed.gate_symbol("SLOW");
        rig.renderer.clear_timeline();
        let done = rig.chart.set_market(to_symbol("SLOW"));
        // Synchronously on the call: old candles gone, affordance up.
        assert!(rig.renderer.bars.borrow().is_empty());
        assert!(rig.renderer.loading.get());
        rig.feed.release();
        rig.block_on(done).expect("switch");
        assert_eq!(close0(&rig), Some(900.0));
        assert!(!rig.renderer.loading.get());
        // The affordance rises BEFORE the blank and falls with the new market's first batch.
        assert_eq!(
            *rig.renderer.timeline.borrow(),
            ["loading:true", "bars:0", "bars:50", "loading:false"]
        );
    }

    #[test]
    fn load_start_and_load_end_bracket_a_switch_for_plugins() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        rig.feed.gate_symbol("SLOW");
        let seen: Rc<RefCell<Vec<LoadSeen>>> = Rc::default();
        for name in [ChartEventName::LoadStart, ChartEventName::LoadEnd] {
            let (seen, renderer) = (Rc::clone(&seen), Rc::clone(&rig.renderer));
            rig.chart.events().on(name, move |e| {
                let painted = renderer.bars.borrow().len();
                match e {
                    ChartEvent::LoadStart {
                        symbol,
                        timeframe,
                        first_load,
                    } => seen.borrow_mut().push((
                        format!("start {symbol}"),
                        timeframe.clone(),
                        painted,
                        None,
                        Some(*first_load),
                    )),
                    ChartEvent::LoadEnd {
                        symbol,
                        timeframe,
                        bars,
                    } => seen.borrow_mut().push((
                        format!("end {symbol}"),
                        timeframe.clone(),
                        painted,
                        Some(*bars),
                        None,
                    )),
                    _ => {}
                }
            });
        }
        let done = rig.chart.set_market(MarketSwitch {
            symbol: Some("SLOW".into()),
            timeframe: Some("240".into()),
            ..MarketSwitch::default()
        });
        // Start already out, carrying the NEW identity, with the OLD series still painted:
        // a plugin hides its own visuals before anything is blanked.
        assert_eq!(
            *seen.borrow(),
            [(
                "start SLOW".to_string(),
                "240".to_string(),
                50,
                None,
                Some(false)
            )]
        );
        rig.feed.release();
        rig.block_on(done).expect("switch");
        assert_eq!(seen.borrow().len(), 2, "exactly one end per start");
        assert_eq!(
            seen.borrow()[1],
            (
                "end SLOW".to_string(),
                "240".to_string(),
                50,
                Some(50),
                None
            )
        );
    }

    #[test]
    fn load_end_fires_alone_for_a_failed_load_and_for_a_parked_symbol() {
        let rig = rig_with_prices().feed(|f| f.fail_symbol("DOWN")).build();
        rig.ready().expect("ready");
        rig.feed.gate_symbol("SLOW");
        let ends = |rig: &Rig| -> Vec<usize> {
            rig.log
                .of(ChartEventName::LoadEnd)
                .into_iter()
                .filter_map(|e| match e {
                    ChartEvent::LoadEnd { bars, .. } => Some(bars),
                    _ => None,
                })
                .collect()
        };
        let before = ends(&rig).len();
        let failed = switch_to(&rig, to_symbol("DOWN"));
        assert!(matches!(failed, Err(DataError::Other(_))));
        assert_eq!(ends(&rig)[before..], [0], "the pair still closes");

        let pending = rig.chart.set_market(to_symbol("SLOW"));
        rig.feed.park("SLOW");
        // Parked: closed immediately, not when (if) bars arrive.
        assert_eq!(ends(&rig)[before..], [0, 0]);
        assert_eq!(rig.log.count(ChartEventName::DataUnresolved), 1);
        rig.feed.release();
        rig.block_on(pending)
            .expect("resolves once the parked load lands");
    }

    #[test]
    fn a_failed_load_still_ends_the_affordance() {
        let rig = rig_with_prices().feed(|f| f.fail_symbol("DOWN")).build();
        rig.ready().expect("ready");
        assert!(switch_to(&rig, to_symbol("DOWN")).is_err());
        assert!(
            rig.renderer.bars.borrow().is_empty(),
            "blanked at the switch; nothing came back"
        );
        assert!(!rig.renderer.loading.get());
        // A failed FIRST load rejects ready.
        let first = rig_with_prices().feed(|f| f.fail_symbol("AAA")).build();
        assert!(matches!(first.ready(), Err(DataError::Other(_))));
        assert!(!first.renderer.loading.get());
        assert_eq!(first.log.count(ChartEventName::Ready), 0);
    }

    #[test]
    fn a_parked_first_load_drops_the_affordance_and_resumes_when_the_symbol_lands() {
        let rig = rig_with_prices()
            .symbol("SLOW")
            .feed(|f| f.gate_symbol("SLOW"))
            .build();
        rig.settle();
        assert!(rig.renderer.loading.get(), "the first load is in flight");
        rig.feed.park("SLOW");
        assert!(!rig.renderer.loading.get());
        assert!(rig.renderer.bars.borrow().is_empty());
        // A provider shows up later: the parked load resumes and paints.
        rig.feed.release();
        rig.settle();
        assert_eq!(close0(&rig), Some(900.0));
        assert!(!rig.renderer.loading.get());
        // The unresolved report carried the symbol.
        let seen = rig.log.of(ChartEventName::DataUnresolved);
        assert!(
            matches!(&seen[..], [ChartEvent::DataUnresolved { symbol, .. }] if symbol == "SLOW")
        );
    }

    // ── data unavailable ──

    fn unavailable(symbol: &str, timeframe: &str) -> DataError {
        DataError::from(DataUnavailableError::new(
            DataUnavailableDetails {
                reason: DataUnavailableReason::Timeframe,
                symbol: Some(symbol.to_string()),
                timeframe: Some(timeframe.to_string()),
                finest: Some("1D".to_string()),
                default: Some("1D".to_string()),
            },
            None,
        ))
    }

    #[test]
    fn an_unavailable_market_ends_the_load_reports_once_and_never_goes_live() {
        let rig = Rig::builder()
            .live(true)
            .feed(|f| f.set_load_error(Some(unavailable("AAA", "60"))))
            .build();
        assert_eq!(
            rig.ready(),
            Ok(()),
            "the chart's own refusal is not a failed load"
        );
        let unavailable_events = rig.log.of(ChartEventName::DataUnavailable);
        assert_eq!(unavailable_events.len(), 1);
        match &unavailable_events[0] {
            ChartEvent::DataUnavailable {
                symbol,
                timeframe,
                reason,
                finest,
                default,
            } => {
                assert_eq!((symbol.as_str(), timeframe.as_str()), ("AAA", "60"));
                assert_eq!(*reason, DataUnavailableReason::Timeframe);
                assert_eq!(finest.as_deref(), Some("1D"));
                assert_eq!(default.as_deref(), Some("1D"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(completions(&rig)[0].0, HistoryReason::Aborted);
        assert_eq!(rig.log.count(ChartEventName::LoadEnd), 1);
        assert!(!rig.renderer.loading.get());
        // Nothing streams for a series that will never come.
        assert_eq!(rig.feed.active_subscribers(), 0);
        assert!(
            rig.feed
                .calls()
                .iter()
                .all(|c| !matches!(c, FeedCall::Subscribe(_)))
        );
        assert!(
            rig.platform
                .warnings()
                .iter()
                .any(|w| w.starts_with("[vela] "))
        );
        // The next load clears the refusal.
        rig.feed.set_load_error(None);
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert_eq!(rig.feed.active_subscribers(), 1);
    }

    #[test]
    fn a_refusal_in_the_deep_backfill_ends_the_history_once() {
        let rig = Rig::builder()
            .bars(25_000)
            .feed(|f| {
                f.default_depth.set(40_000);
                f.set_range_error(Some(unavailable("AAA", "60")));
            })
            .build();
        rig.loaded();
        assert_eq!(rig.log.count(ChartEventName::DataUnavailable), 1);
        assert_eq!(completions(&rig).len(), 1);
        assert_eq!(completions(&rig)[0].0, HistoryReason::Aborted);
        assert_eq!(rig.renderer.bars.borrow().len(), 10_000);
    }

    // ── symbol facts, price format, volume auto-add ──

    #[test]
    fn symbol_facts_reach_the_renderer_and_the_bus_and_reset_on_a_symbol_switch() {
        let rig = rig_with_prices()
            .feed(|f| {
                f.set_symbol_info_for(
                    "AAA",
                    SymbolInfo {
                        ticker: "AAA".into(),
                        ohlc: Some(false),
                        ..SymbolInfo::default()
                    },
                );
            })
            .build();
        rig.ready().expect("ready");
        let facts = SymbolFacts {
            ohlc: Some(false),
            volume: None,
        };
        assert_eq!(rig.chart.symbol_facts(), Some(facts));
        assert_eq!(*rig.renderer.facts_calls.borrow(), [Some(facts)]);
        // The probe ran in the constructor's synchronous prefix: its event went out
        // before a host could subscribe, as in the original.
        assert_eq!(rig.log.count(ChartEventName::SymbolFacts), 0);

        // A symbol nothing states facts for starts from unknown again.
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert_eq!(rig.chart.symbol_facts(), None);
        assert_eq!(rig.renderer.facts_calls.borrow().last(), Some(&None));
        let seen = rig.log.of(ChartEventName::SymbolFacts);
        assert!(
            matches!(&seen[..], [ChartEvent::SymbolFacts { symbol, facts: None }] if symbol == "BBB")
        );
    }

    #[test]
    fn a_price_axis_tick_size_reaches_a_renderer_that_takes_it() {
        let info: SymbolInfo =
            serde_json::from_value(json!({"ticker": "AAA", "mintick": 0.01})).expect("symbol info");
        let rig = Rig::builder()
            .price_precision(true)
            .feed(move |f| f.set_symbol_info(Some(info)))
            .build();
        rig.ready().expect("ready");
        rig.settle();
        assert_eq!(
            rig.renderer.precision_calls.borrow().last(),
            Some(&Some(0.01))
        );
        // A non-positive tick size reads as unknown.
        let zero: SymbolInfo =
            serde_json::from_value(json!({"ticker": "AAA", "mintick": 0})).expect("symbol info");
        let rig = Rig::builder()
            .price_precision(true)
            .feed(move |f| f.set_symbol_info(Some(zero)))
            .build();
        rig.ready().expect("ready");
        assert_eq!(rig.renderer.precision_calls.borrow().last(), Some(&None));
        // A renderer without the seam is never asked.
        let plain = Rig::builder().build();
        plain.ready().expect("ready");
        assert!(plain.renderer.precision_calls.borrow().is_empty());
    }

    fn handle_types(rig: &Rig) -> Vec<Option<String>> {
        rig.chart
            .list_indicators()
            .iter()
            .map(|h| h.native_type().map(str::to_string))
            .collect()
    }

    #[test]
    fn the_volume_auto_add_follows_the_option_the_layer_and_the_symbol_facts() {
        // Default off here; on adds one.
        let on = Rig::builder().volume(true).build();
        on.ready().expect("ready");
        assert_eq!(handle_types(&on), [Some("volume".to_string())]);
        let off = Rig::builder().build();
        off.ready().expect("ready");
        assert!(handle_types(&off).is_empty());
        // No native layer: nothing to add.
        let no_layer = Rig::builder().volume(true).native_data(false).build();
        no_layer.ready().expect("ready");
        assert!(handle_types(&no_layer).is_empty());
        // A symbol whose source states no volume.
        let macro_symbol = Rig::builder()
            .volume(true)
            .feed(|f| {
                f.set_symbol_info(Some(SymbolInfo {
                    ticker: "AAA".into(),
                    volume: Some(false),
                    ..SymbolInfo::default()
                }));
            })
            .build();
        macro_symbol.ready().expect("ready");
        assert!(handle_types(&macro_symbol).is_empty());
        assert!(macro_symbol.chart.pending_native_indicators().is_empty());
        // A symbol whose facts say nothing about volume still gets it.
        let quiet = Rig::builder()
            .volume(true)
            .feed(|f| {
                f.set_symbol_info(Some(SymbolInfo {
                    ticker: "AAA".into(),
                    ..SymbolInfo::default()
                }))
            })
            .build();
        quiet.ready().expect("ready");
        assert_eq!(handle_types(&quiet), [Some("volume".to_string())]);
    }

    #[test]
    fn a_volume_the_user_removed_is_never_auto_added_again() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        rig.chart.inner.state.borrow_mut().config.volume = Some(true);
        rig.chart.inner.state.borrow_mut().volume_opted_out = true;
        rig.chart.maybe_auto_add_volume();
        assert!(handle_types(&rig).is_empty());
    }

    #[test]
    fn the_volume_auto_add_rides_the_first_candles_only() {
        let rig = rig_with_prices().volume(true).build();
        rig.ready().expect("ready");
        assert_eq!(handle_types(&rig).len(), 1);
        // A market switch must not re-add what the user has since removed.
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert_eq!(handle_types(&rig).len(), 1);
    }

    // ── misc surface ──

    #[test]
    fn the_execution_market_carries_the_symbol_timeframe_style_and_session() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let m = rig.chart.market();
        assert_eq!((m.symbol.as_str(), m.timeframe.as_str()), ("AAA", "60"));
        assert_eq!(m.chart_style.as_deref(), Some("candles"));
        assert_eq!(m.session, None);
        switch_to(
            &rig,
            MarketSwitch {
                session: Some(MarketSession::Extended),
                ..MarketSwitch::default()
            },
        )
        .expect("switch");
        assert_eq!(rig.chart.market().session.as_deref(), Some("extended"));
    }

    #[test]
    fn the_engine_lookup_defaults_to_the_chart_language_and_fails_with_an_actionable_error() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        assert!(rig.chart.engine_for(None).is_ok());
        assert!(rig.chart.engine_for(Some("pine")).is_ok());
        let err = rig
            .chart
            .engine_for(Some("lua"))
            .err()
            .expect("no lua engine");
        assert!(
            err.message
                .contains("no scripting engine registered for language \"lua\"")
        );
        assert!(err.message.contains("registerEngine"));
        // The last registration for a language wins, with a warning on a replacement.
        let other = FakeEngine::new();
        rig.chart
            .register_engine("pine", Rc::clone(&other) as Rc<dyn ScriptingEngine>);
        assert_eq!(rig.platform.warnings().len(), 1);
        assert!(
            rig.platform.warnings()[0]
                .contains("re-registering scripting engine for language \"pine\"")
        );
        let same: Rc<dyn ScriptingEngine> = Rc::clone(&other) as Rc<dyn ScriptingEngine>;
        rig.chart.register_engine("pine", same);
        assert_eq!(
            rig.platform.warnings().len(),
            1,
            "the same engine again is silent"
        );
    }

    #[test]
    fn inspect_lists_the_mounted_models_and_skips_records_still_loading() {
        let rig = Rig::builder().build();
        rig.ready().expect("ready");
        assert!(rig.chart.inspect().indicators.is_empty());
        rig.attach_session("ind-1", "overlay=true");
        assert_eq!(rig.chart.inspect().indicators.len(), 1);
        rig.record("ind-1").borrow_mut().loading = true;
        assert!(rig.chart.inspect().indicators.is_empty());
    }

    #[test]
    fn a_resize_reaches_the_renderer() {
        let rig = Rig::builder().build();
        rig.chart.resize();
    }

    // ── re-entrancy and lifecycle ──

    #[test]
    fn a_renderer_that_calls_back_into_the_chart_from_set_bars_does_not_deadlock() {
        let rig = rig_with_prices().renderer(|_| {}).build();
        rig.ready().expect("ready");
        let chart = rig.chart.clone();
        let renderer = Rc::clone(&rig.renderer);
        let touched = Rc::new(Cell::new(0));
        let counter = Rc::clone(&touched);
        *rig.renderer.set_bars_hook.borrow_mut() = Some(Rc::new(move || {
            // Every read and write a host might do from a renderer callback.
            let _ = chart.market_snapshot();
            let _ = chart.symbol_facts();
            let _ = chart.inspect();
            let _ = chart.view_bars();
            chart.set_theme(light_theme());
            renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
            counter.set(counter.get() + 1);
        }));
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        assert!(
            touched.get() >= 2,
            "the hook ran on the blank and on the paint"
        );
        assert_eq!(close0(&rig), Some(200.0));
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
    }

    #[test]
    fn an_event_handler_that_switches_the_market_again_lands_on_the_last_switch() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let chart = rig.chart.clone();
        let nested = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&nested);
        rig.chart
            .events()
            .on(ChartEventName::MarketChanged, move |e| {
                if let ChartEvent::MarketChanged { symbol, .. } = e
                    && symbol == "BBB"
                {
                    *slot.borrow_mut() = Some(chart.set_market(to_symbol("CCC")));
                }
            });
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        let follow_up = nested.borrow_mut().take().expect("the handler ran");
        rig.block_on(follow_up).expect("the nested switch");
        assert_eq!(close0(&rig), Some(300.0));
        assert_eq!(rig.chart.market_snapshot().symbol.as_deref(), Some("CCC"));
        let symbols: Vec<_> = market_changed(&rig).into_iter().map(|e| e.0).collect();
        assert_eq!(symbols, ["BBB", "CCC"]);
        rig.chart.destroy();
    }

    #[test]
    fn a_bar_handler_may_read_the_chart() {
        let rig = Rig::builder()
            .live(true)
            .feed(|f| f.default_depth.set(10))
            .build();
        rig.ready().expect("ready");
        let chart = rig.chart.clone();
        let seen = Rc::new(Cell::new(0usize));
        let counter = Rc::clone(&seen);
        rig.chart.events().on(ChartEventName::Bar, move |_| {
            counter.set(chart.view_bars().len());
        });
        rig.feed.push_tick(bar_at(T0 + 10 * HOUR, 1.0));
        assert_eq!(
            seen.get(),
            11,
            "the bar is already on the chart when the event fires"
        );
        rig.chart.destroy();
    }

    #[test]
    fn a_response_landing_after_destroy_paints_nothing_and_emits_nothing() {
        let rig = Rig::builder().feed(|f| f.gate_loads(true)).build();
        rig.settle();
        rig.chart.destroy();
        rig.feed.release();
        rig.settle();
        assert!(rig.renderer.set_bars_calls.borrow().is_empty());
        assert_eq!(rig.log.count(ChartEventName::Ready), 0);
        assert_eq!(rig.log.count(ChartEventName::LoadEnd), 0);
        // ready still resolves: the pipeline ended (superseded), it did not fail.
        assert_eq!(rig.ready(), Ok(()));
    }

    #[test]
    fn dropping_the_last_handle_frees_the_chart_and_late_callbacks_are_inert() {
        let rig = Rig::builder().live(true).build();
        rig.ready().expect("ready");
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        let Rig {
            platform,
            renderer,
            feed,
            chart,
            engine,
            log,
            registries,
        } = rig;
        let weak = chart.weak();
        drop(chart);
        drop((engine, registries));
        // Nothing the chart registered on a long-lived collaborator keeps it alive.
        assert!(weak.upgrade().is_none());
        let before = log.all().len();
        platform.advance(VIEWPORT_DEBOUNCE_MS + 1.0); // the debounce timer fires on a dead chart
        renderer.fire_viewport(VisibleRange { from: 3, to: 4 });
        renderer.fire_price_style("heikinashi");
        renderer.fire_remove("nope");
        feed.push_tick(bar_at(T0 + 60 * HOUR, 1.0));
        feed.park("AAA");
        assert_eq!(log.all().len(), before);
    }

    #[test]
    fn a_destroyed_chart_ignores_what_the_renderer_still_delivers() {
        let rig = Rig::builder().live(true).build();
        rig.ready().expect("ready");
        let session = rig.attach_session("vr", "overlay=true visible");
        rig.chart.destroy();
        assert_eq!(session.stops(), 1, "sessions are stopped at destroy");
        assert!(!rig.chart.events().has_listeners(&ChartEventName::Bar));
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(session.visible_ranges().is_empty());
        // The theme can still be asked for; the bus is empty, nobody hears it.
        rig.chart.set_theme(light_theme());
        assert_eq!(rig.log.count(ChartEventName::ThemeChanged), 0);
    }

    #[test]
    fn the_generation_only_moves_forward_and_a_superseded_load_leaves_the_chart_alone() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let g0 = rig.chart.generation();
        rig.feed.gate_symbol("SLOW");
        let slow = rig.chart.set_market(to_symbol("SLOW"));
        assert_eq!(rig.chart.generation(), g0 + 1);
        let fast = rig.chart.set_market(to_symbol("BBB"));
        assert_eq!(rig.chart.generation(), g0 + 2);
        rig.block_on(fast).expect("fast");
        rig.block_on(slow).expect("slow");
        // A stale load that lands now must not touch loading state either.
        let ends = rig.log.count(ChartEventName::LoadEnd);
        rig.feed.release();
        rig.settle();
        assert_eq!(rig.log.count(ChartEventName::LoadEnd), ends);
        rig.chart.destroy();
        assert_eq!(rig.chart.generation(), g0 + 3);
    }

    #[test]
    fn a_pending_viewport_timer_from_the_old_market_is_dropped_by_a_switch() {
        let rig = rig_with_prices().build();
        rig.ready().expect("ready");
        let vr = rig.attach_session("vr", "overlay=true visible");
        rig.renderer.fire_viewport(VisibleRange { from: 1, to: 2 });
        switch_to(&rig, to_symbol("BBB")).expect("switch");
        rig.advance(VIEWPORT_DEBOUNCE_MS + 1.0);
        assert!(
            vr.visible_ranges().is_empty(),
            "the old market's window means nothing on the new one"
        );
        assert_eq!(rig.chart.inner.state.borrow().visible_range, None);
    }
}
