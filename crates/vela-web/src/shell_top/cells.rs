//! The per-cell overlays: for every live cell, the status line, the watermark, the hover
//! controls and the two trackers that keep the market badge and the session shading honest
//! (what `ChartCell.ts` builds into each cell's host).
//!
//! [`CellOverlay`] binds one cell container to its cell: it listens to the cell's chart
//! (`bar`, `replay:*`, `load:*`, `market:changed`, `viewport:changed`, `priceStyle:change`,
//! `locale:changed`), to the renderer (crosshair, config, axis long-press) and projects what
//! it hears onto the overlays. The shell creates one per `cell:created` and drops it on
//! `cell:destroyed`.
//!
//! Differences from the original: the core cell has no event for a status-prefs change on a
//! cell that is not the active one, so the overlay re-reads
//! [`ChartCell::status_prefs`] on the events it does get (the projection of its cell, the
//! chart's market and locale events, the state-changed pulse) and straight after its own
//! right-click toggles.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use serde_json::{Value, json};
use vela_core::events::Subscription;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::model::Ohlcv;
use vela_core::ports::chart_renderer::{Axis, AxisLongPressEvent, CrosshairEvent};
use vela_core::runtime::Unsubscribe;
use vela_core::vela::Vela;
use vela_core::widget::market_status::MarketStatusTracker;
use vela_core::widget::session_shading::{SessionShadingTracker, TrackOptions};
use vela_core::widget::statusline::{BarLike, RendererReads, statusline_ink_of};
use vela_core::widget::statusline_model::{StatuslinePart, StatuslineParts};
use vela_core::workspace::cell::{StatuslinePart as CellPart, StatuslinePartsOn};
use vela_core::workspace::{ChartCell, WeakChartCell, WeakWorkspace, Workspace};
use web_sys::Element;

use super::cell_controls::{CellControls, CellControlsDeps};
use super::kit::Env;
use super::statusline::{Statusline, StatuslineMenuHooks};
use super::watermark::Watermark;

/// What an overlay asks of the shell.
#[derive(Clone)]
pub struct CellHooks {
    /// A touch long-press on the time axis opens the time-zone drawer.
    pub open_timezone_drawer: Rc<dyn Fn()>,
}

fn bar_of(b: &Ohlcv) -> BarLike {
    BarLike {
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
    }
}

struct RReads(Vela);

impl RendererReads for RReads {
    fn get_config(&self) -> Option<Value> {
        self.0.renderer().get_config()
    }
    fn get(&self, key: &str) -> Option<Value> {
        self.0.renderer().get(key)
    }
}

fn parts_of(on: &StatuslinePartsOn) -> StatuslineParts {
    StatuslineParts {
        logo: on.logo,
        name: on.name,
        market: on.market,
        ohlc: on.ohlc,
        change: on.change,
    }
}

fn cell_part(part: StatuslinePart) -> CellPart {
    match part {
        StatuslinePart::Logo => CellPart::Logo,
        StatuslinePart::Name => CellPart::Name,
        StatuslinePart::Market => CellPart::Market,
        StatuslinePart::Ohlc => CellPart::Ohlc,
        StatuslinePart::Change => CellPart::Change,
    }
}

pub struct CellOverlay {
    env: Env,
    me: Weak<CellOverlay>,
    id: String,
    ws: WeakWorkspace,
    cell: WeakChartCell,
    pub statusline: Option<Rc<Statusline>>,
    pub watermark: Option<Rc<Watermark>>,
    pub controls: Rc<CellControls>,
    market: RefCell<Option<MarketStatusTracker>>,
    session: RefCell<Option<SessionShadingTracker>>,
    session_key: RefCell<String>,
    chart_subs: RefCell<Vec<Subscription<ChartEventName>>>,
    chart: RefCell<Option<Vela>>,
    unsubs: RefCell<Vec<Unsubscribe>>,
    destroyed: Cell<bool>,
}

impl CellOverlay {
    /// Build the overlays of cell `id` inside its container. `None` when the cell or its
    /// chart is gone.
    pub fn attach(
        env: &Env,
        ws: &Workspace,
        id: &str,
        container: &Element,
        hooks: CellHooks,
    ) -> Option<Rc<CellOverlay>> {
        let cell = ws.cell(id)?;
        let chart = cell.try_chart()?;
        let root = env.root()?;
        let shell = root.shell_options();
        let prefs = cell.status_prefs();
        let symbol = cell.symbol();
        let timeframe = cell.timeframe();

        // The watermark sits beneath the status line in paint order: it goes in first.
        let watermark = (shell.watermark != Some(false))
            .then(|| Watermark::new(env, container, &symbol, &timeframe));
        let weak_cell = cell.downgrade();
        let icon_for: Rc<dyn Fn(&str) -> Option<String>> = {
            let wc = weak_cell.clone();
            Rc::new(move |sym| wc.upgrade()?.try_chart()?.data().symbol_icon(sym))
        };
        let statusline = (shell.statusline != Some(false) && prefs.parts.is_some())
            .then(|| Statusline::new(env, container, &symbol, icon_for));

        let controls = {
            let (wc, wws, rid) = (weak_cell.clone(), ws.downgrade(), id.to_string());
            let chart_of = {
                let wc = wc.clone();
                Rc::new(move || wc.upgrade()?.try_chart()) as Rc<dyn Fn() -> Option<Vela>>
            };
            let ws_of = {
                let w = wws.clone();
                move || w.upgrade()
            };
            let (a, b, c, d, e, f) = (
                ws_of.clone(),
                ws_of.clone(),
                ws_of.clone(),
                ws_of.clone(),
                ws_of.clone(),
                ws_of.clone(),
            );
            let (rid_a, rid_b, rid_c) = (rid.clone(), rid.clone(), rid.clone());
            let wcr = wc.clone();
            let root_w = env.root.clone();
            CellControls::new(
                env,
                container,
                CellControlsDeps {
                    chart: chart_of,
                    reset: Rc::new(move || {
                        if let Some(cell) = wcr.upgrade() {
                            cell.reset_view();
                        }
                    }),
                    multi_cell: Rc::new(move || a().is_some_and(|w| w.cells().len() > 1)),
                    is_maximized: Rc::new(move || {
                        b().is_some_and(|w| w.maximized_cell().as_deref() == Some(rid_a.as_str()))
                    }),
                    toggle_maximize: Rc::new(move || {
                        if let Some(w) = c() {
                            if w.maximized_cell().as_deref() == Some(rid_b.as_str()) {
                                w.maximize_cell(None);
                            } else {
                                w.maximize_cell(Some(&rid_b));
                            }
                        }
                    }),
                    drag_target_at: Rc::new(move |x, y| {
                        // The OTHER live cell under the point (never this one, never one
                        // hidden under a maximized cell).
                        let root = root_w.upgrade()?;
                        let ws = d()?;
                        let maximized = ws.maximized_cell();
                        root.cell_containers().into_iter().find_map(|(cid, el)| {
                            if cid == rid_c || maximized.is_some() {
                                return None;
                            }
                            let r = el.get_bounding_client_rect();
                            (x >= r.left() && x < r.right() && y >= r.top() && y < r.bottom())
                                .then_some(cid)
                        })
                    }),
                    preview_drop: Rc::new(move |t| {
                        if let Some(w) = e() {
                            w.set_drop_target(t);
                        }
                    }),
                    drop_on: Rc::new({
                        let rid = rid.clone();
                        move |target| {
                            if let Some(w) = f() {
                                w.swap_cells(&rid, target);
                            }
                        }
                    }),
                },
            )
        };
        let this = Rc::new_cyclic(|me| CellOverlay {
            env: env.clone(),
            me: me.clone(),
            id: id.to_string(),
            ws: ws.downgrade(),
            cell: weak_cell,
            statusline,
            watermark,
            controls,
            market: RefCell::new(None),
            session: RefCell::new(None),
            session_key: RefCell::new(String::new()),
            chart_subs: RefCell::new(Vec::new()),
            chart: RefCell::new(Some(chart.clone())),
            unsubs: RefCell::new(Vec::new()),
            destroyed: Cell::new(false),
        });
        this.wire(&cell, &chart, hooks);
        Some(this)
    }

    fn cell(&self) -> Option<ChartCell> {
        self.cell.upgrade()
    }

    fn wire(&self, cell: &ChartCell, chart: &Vela, hooks: CellHooks) {
        // Prefs first: the statusline's parts, the watermark toggles, the menu hooks.
        if let Some(sl) = &self.statusline {
            let (wc, me) = (self.cell.clone(), self.me.clone());
            let (wc2, wc3) = (self.cell.clone(), self.cell.clone());
            sl.attach_menu(StatuslineMenuHooks {
                set_part: Rc::new(move |part, visible| {
                    if let Some(c) = wc.upgrade() {
                        c.set_statusline_part(cell_part(part), visible);
                    }
                    if let Some(o) = me.upgrade() {
                        o.sync_prefs();
                    }
                }),
                chart_visible: Rc::new(move || {
                    wc2.upgrade()
                        .and_then(|c| c.try_chart())
                        .is_none_or(|ch| ch.renderer().get("candleVisible") != Some(json!(false)))
                }),
                set_chart_visible: Rc::new(move |v| {
                    if let Some(ch) = wc3.upgrade().and_then(|c| c.try_chart()) {
                        ch.renderer().set("candleVisible", json!(v));
                    }
                }),
            });
        }
        self.sync_prefs();
        self.refresh_identity();
        self.sync_colors();
        if let Some(w) = &self.watermark {
            w.set_loading(cell.loading());
            w.set_replaying(chart.replay().state().active);
        }
        if let Some(sl) = &self.statusline {
            sl.set_replaying(chart.replay().state().active);
        }
        // The market badge.
        if let Some(sl) = &self.statusline {
            let sl2 = sl.clone();
            *self.market.borrow_mut() = Some(MarketStatusTracker::new(
                self.env.platform.clone(),
                self.intl(),
                move |s| sl2.set_market_status(s),
            ));
        }
        // The session shading: the bands go into the renderer's `sessionZones` feature.
        {
            let wc = self.cell.clone();
            *self.session.borrow_mut() = Some(SessionShadingTracker::new(
                self.env.platform.clone(),
                self.intl(),
                move |zones| {
                    let Some(ch) = wc.upgrade().and_then(|c| c.try_chart()) else {
                        return;
                    };
                    let pairs = |w: &[(i64, i64)]| -> Vec<Value> {
                        w.iter().map(|(a, b)| json!([a, b])).collect()
                    };
                    let value = match zones {
                        Some(z) => json!({
                            "pre": pairs(&z.pre),
                            "post": pairs(&z.post),
                            "extended": pairs(&z.extended),
                        }),
                        None => Value::Null,
                    };
                    ch.renderer().set("sessionZones", value);
                },
            ));
        }
        self.track_market();
        self.track_session(true);
        // The venue chip and the avatar re-resolve once the shared feed's indexes settle.
        {
            let (me, ready) = (self.me.clone(), chart.data().ready());
            self.env.platform.spawn(Box::pin(async move {
                ready.await;
                if let Some(o) = me.upgrade()
                    && !o.destroyed.get()
                {
                    o.refresh_identity();
                    o.track_market();
                    o.track_session(true);
                }
            }));
        }

        // The chart's events.
        let mut subs = Vec::new();
        let on = |name: ChartEventName, f: Box<dyn Fn(&Rc<CellOverlay>, &ChartEvent)>| {
            let me = self.me.clone();
            chart.on(name, move |ev| {
                if let Some(o) = me.upgrade()
                    && !o.destroyed.get()
                {
                    f(&o, ev);
                }
            })
        };
        subs.push(on(
            ChartEventName::Bar,
            Box::new(|o, ev| {
                if let (Some(sl), ChartEvent::Bar(b)) = (&o.statusline, ev) {
                    sl.set_last_bar(Some(bar_of(b)));
                }
            }),
        ));
        subs.push(on(
            ChartEventName::ReplayStart,
            Box::new(|o, _| o.replay(true)),
        ));
        subs.push(on(
            ChartEventName::ReplayEnd,
            Box::new(|o, _| o.replay(false)),
        ));
        subs.push(on(
            ChartEventName::LoadStart,
            Box::new(|o, _| {
                if let Some(w) = &o.watermark {
                    w.set_loading(true);
                }
            }),
        ));
        subs.push(on(
            ChartEventName::LoadEnd,
            Box::new(|o, _| {
                if let Some(w) = &o.watermark {
                    w.set_loading(false);
                }
            }),
        ));
        subs.push(on(
            ChartEventName::MarketChanged,
            Box::new(|o, _| {
                // The original re-binds the status line to its chart here (`onChart`), which
                // drops the old market's resting and hover readouts.
                if let Some(sl) = &o.statusline {
                    sl.clear_bars();
                }
                o.refresh_identity();
                o.sync_prefs();
                o.track_market();
                o.track_session(false);
            }),
        ));
        subs.push(on(
            ChartEventName::ViewportChanged,
            Box::new(|o, ev| {
                if let (Some(t), ChartEvent::ViewportChanged { from, to }) =
                    (o.session.borrow().as_ref(), ev)
                {
                    t.update_range(vela_core::indicators::VisibleRange {
                        from: *from,
                        to: *to,
                    });
                }
            }),
        ));
        // `priceStyle:change` is announced BEFORE the chart shows the new style (the chart
        // still reports `from` while it runs), so the ink is derived for `to` explicitly.
        subs.push(on(
            ChartEventName::PriceStyleChange,
            Box::new(|o, ev| match ev {
                ChartEvent::PriceStyleChange { to, .. } => o.sync_colors_for(Some(to)),
                _ => o.sync_colors(),
            }),
        ));
        // A symbol's facts may change which style is shown (the original re-derives the ink).
        subs.push(on(
            ChartEventName::SymbolFacts,
            Box::new(|o, _| o.sync_colors()),
        ));
        subs.push(on(
            ChartEventName::LocaleChanged,
            Box::new(|o, _| o.relocalize()),
        ));
        *self.chart_subs.borrow_mut() = subs;

        // The renderer's seams.
        let mut unsubs: Vec<Unsubscribe> = Vec::new();
        let me = self.me.clone();
        unsubs.push(
            chart
                .renderer()
                .on_crosshair_move(Box::new(move |e: &CrosshairEvent| {
                    if let Some(o) = me.upgrade()
                        && let Some(sl) = &o.statusline
                    {
                        sl.set_hover_bar(e.ohlc.as_ref().map(|b| BarLike {
                            open: b.open,
                            high: b.high,
                            low: b.low,
                            close: b.close,
                        }));
                    }
                })),
        );
        let me = self.me.clone();
        unsubs.push(chart.renderer().on_config_changed(Box::new(move || {
            if let Some(o) = me.upgrade() {
                o.sync_colors(); // a settings edit may have recolored the active style
            }
        })));
        let open = hooks.open_timezone_drawer;
        unsubs.push(chart.renderer().on_axis_long_press(Box::new(
            move |e: &AxisLongPressEvent| {
                if e.axis == Axis::Time {
                    open();
                }
            },
        )));
        *self.unsubs.borrow_mut() = unsubs;
    }

    fn intl(&self) -> std::sync::Arc<dyn vela_core::intl::Intl> {
        self.env
            .root()
            .map(|r| r.env().intl.clone())
            .expect("an overlay is built while the root lives")
    }

    fn replay(&self, on: bool) {
        if let Some(sl) = &self.statusline {
            sl.set_last_bar(None);
            sl.set_replaying(on);
        }
        if let Some(w) = &self.watermark {
            w.set_replaying(on);
        }
    }

    /// Symbol, venue and timeframe: the status line and the watermark read the cell.
    pub fn refresh_identity(&self) {
        let Some(cell) = self.cell() else { return };
        let (symbol, timeframe) = (cell.symbol(), cell.timeframe());
        let prefix = cell
            .try_chart()
            .and_then(|c| c.data().display_prefix(&symbol))
            .or_else(|| cell.provider())
            .unwrap_or_default();
        if let Some(w) = &self.watermark {
            w.update(&symbol, &timeframe);
        }
        if let Some(sl) = &self.statusline {
            sl.set_symbol(&symbol);
            sl.set_meta(&timeframe, &prefix);
        }
    }

    /// The Status line tab and watermark prefs of the cell, onto the overlays.
    pub fn sync_prefs(&self) {
        let Some(cell) = self.cell() else { return };
        let prefs = cell.status_prefs();
        if let (Some(sl), Some(parts)) = (&self.statusline, prefs.parts) {
            sl.set_parts(parts_of(&parts));
        }
        if let Some(w) = &self.watermark {
            w.set_visible(prefs.watermark);
            w.set_replay_visible(prefs.replay_watermark);
        }
    }

    /// The up/down ink and the readout shape of the active price style.
    pub fn sync_colors(&self) {
        self.sync_colors_for(None);
    }

    /// [`sync_colors`](Self::sync_colors) for a style that is about to be shown.
    fn sync_colors_for(&self, style: Option<&str>) {
        let (Some(sl), Some(cell)) = (&self.statusline, self.cell()) else {
            return;
        };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let style = style.map_or_else(|| cell.price_style(), str::to_string);
        let ink = statusline_ink_of(Rc::new(RReads(chart)), &style);
        sl.set_direction_colors(ink);
    }

    fn track_market(&self) {
        let Some(cell) = self.cell() else { return };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        if let Some(tracker) = self.market.borrow().as_ref() {
            tracker.track(chart.data(), &cell.symbol());
        }
    }

    /// (Re)derive the session shading for the cell's market; `force` re-tracks even when the
    /// market identity did not change.
    fn track_session(&self, force: bool) {
        let Some(cell) = self.cell() else { return };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let (symbol, timeframe, session) = (cell.symbol(), cell.timeframe(), cell.session());
        let key = format!("{symbol}|{timeframe}|{}", session.as_str());
        if !force && *self.session_key.borrow() == key {
            return;
        }
        *self.session_key.borrow_mut() = key;
        let now = self.env.platform.now_ms() as i64;
        let range = chart
            .get_visible_range()
            .unwrap_or(vela_core::indicators::VisibleRange {
                from: now - 3 * 86_400_000,
                to: now,
            });
        if let Some(t) = self.session.borrow().as_ref() {
            t.track(
                chart.data(),
                &symbol,
                TrackOptions {
                    session,
                    timeframe,
                    range,
                },
            );
        }
    }

    /// The projection of this cell, the grid's shape, the maximize state or the session
    /// changed: re-read what the overlays show.
    pub fn on_projection(&self) {
        self.refresh_identity();
        self.sync_prefs();
        self.sync_colors();
        self.track_session(false);
        self.controls.refresh();
    }

    /// The chrome's size class flipped: the mobile bar's maximize stop replaces the cluster.
    pub fn set_mobile(&self, mobile: bool) {
        self.controls.set_suspended(mobile);
    }

    /// Multi-chart cells keep the status line on one row.
    pub fn set_fit_mode(&self, on: bool) {
        if let Some(sl) = &self.statusline {
            sl.set_fit_mode(on);
        }
    }

    pub fn relocalize(&self) {
        if let Some(sl) = &self.statusline {
            sl.relocalize();
        }
        if let Some(w) = &self.watermark {
            w.relocalize();
        }
        self.controls.refresh();
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        if let Some(chart) = self.chart.borrow_mut().take() {
            for sub in self.chart_subs.borrow_mut().drain(..) {
                chart.off(&sub);
            }
        }
        for u in self.unsubs.borrow_mut().drain(..) {
            u();
        }
        if let Some(t) = self.market.borrow_mut().take() {
            t.stop();
        }
        if let Some(t) = self.session.borrow_mut().take() {
            t.stop();
        }
        self.controls.destroy();
        if let Some(sl) = &self.statusline {
            sl.destroy();
        }
        if let Some(w) = &self.watermark {
            w.destroy();
        }
        let _ = &self.ws;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_parts_map_onto_the_widget_models_parts() {
        let on = StatuslinePartsOn {
            logo: true,
            name: false,
            market: true,
            ohlc: false,
            change: true,
        };
        let parts = parts_of(&on);
        assert!(parts.logo && !parts.name && parts.market && !parts.ohlc && parts.change);
        for part in StatuslinePart::ALL {
            assert_eq!(cell_part(part).as_str(), part.as_str());
        }
    }

    #[test]
    fn a_bar_reads_its_four_prices() {
        let bar = bar_of(&Ohlcv {
            time: 1,
            open: 1.0,
            high: 3.0,
            low: 0.5,
            close: 2.0,
            volume: None,
        });
        assert_eq!(
            (bar.open, bar.high, bar.low, bar.close),
            (1.0, 3.0, 0.5, 2.0)
        );
    }
}
