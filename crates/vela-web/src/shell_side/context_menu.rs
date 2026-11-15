//! The chart context menu (`context-menu.ts` on the kit's `ContextMenu`): a right-click on a
//! chart opens a menu whose rows depend on the zone it landed in (price axis, time axis, the
//! body) and on the renderer's current feature states, with the contributed `context:*`
//! rows interleaved. A touch long-press is NOT a menu here (it is the chart's touch
//! crosshair), as in the original (`longPress: false`).
//!
//! One `contextmenu` listener on the grid serves every cell: the cell under the event is
//! found from its `vela-cell` container, so the listener sits ABOVE the chart's own, and
//! cooperates with it: a right-click that cancelled a drawing placement was already
//! `preventDefault`-ed and `stopPropagation`-ed by the chart (`input.rs`), which this listener
//! therefore never sees; a defaulted event is left alone.
//!
//! The rows and every rule are `vela_core::widget::{context_menu, context_menu_model}`: the
//! zone from [`zone_of`], the open-time pointer in a [`ContextMenuState`], the rows from the
//! model builders and [`compose_menu`], a selected id through [`route_selection`] to a
//! [`MenuCommand`] applied here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use serde_json::{Value, json};
use vela_core::model::PaneKind as ModelPaneKind;
use vela_core::widget::context_menu::{
    ContextMenuState, MenuCommand, contributed_rows, route_selection, target_of, zone_of,
};
use vela_core::widget::context_menu_model::{
    PaneKind, PaneScaleInfo, PriceAxisState, ScaleChoice, Zone, body_items, compose_menu,
    invert_write, price_axis_items, price_only_state, scale_choice_of, scale_writes,
    time_axis_items,
};
use vela_core::widget::contributions::{Contributions, PointerPaneKind, WidgetContext};

use crate::workspace_ui::RootContext;
use vela_core::workspace::{Workspace, WorkspaceEventName};
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::dom::{get_bool, prevent_default};

use super::bind::ChartSubs;
use super::kit::{Bag, Cx, Disposers, closest, data, ev_f64, event_target, has_class};
use super::menu::{MenuHandle, open_menu};

struct CellState {
    state: ContextMenuState,
}

pub struct ContextMenus {
    cx: Cx,
    me: Weak<ContextMenus>,
    ws: vela_core::workspace::WeakWorkspace,
    contributions: Option<Contributions>,
    context: Rc<dyn Fn() -> Rc<RootContext>>,
    cells: RefCell<HashMap<String, Rc<RefCell<CellState>>>>,
    subs: RefCell<HashMap<String, Box<dyn FnOnce()>>>,
    bag: Rc<Bag>,
    menu: RefCell<Option<Rc<MenuHandle>>>,
    /// The cell, zone and context of the open menu (what `run` reads).
    open: RefCell<Option<(String, Zone, Rc<RootContext>)>>,
    _chart_subs: ChartSubs,
}

impl ContextMenus {
    pub fn new(
        cx: &Cx,
        ws: &Workspace,
        contributions: Option<Contributions>,
        context: Rc<dyn Fn() -> Rc<RootContext>>,
        disposers: &Disposers,
    ) -> Rc<ContextMenus> {
        let this = Rc::new_cyclic(|me| ContextMenus {
            cx: cx.clone(),
            me: me.clone(),
            ws: ws.downgrade(),
            contributions,
            context,
            cells: RefCell::default(),
            subs: RefCell::default(),
            bag: Bag::new(),
            menu: RefCell::new(None),
            open: RefCell::new(None),
            _chart_subs: ChartSubs::default(),
        });
        this.sync_cells();
        for name in [
            WorkspaceEventName::CellCreated,
            WorkspaceEventName::CellDestroyed,
            WorkspaceEventName::LayoutChanged,
            // A cell whose chart was not ready at its creation is picked up at the next one.
            WorkspaceEventName::CellActive,
            WorkspaceEventName::Projection,
        ] {
            let me = this.me.clone();
            let sub = ws.on(name, move |_| {
                if let Some(m) = me.upgrade() {
                    m.sync_cells();
                }
            });
            let weak = ws.downgrade();
            disposers.push(move || {
                if let Some(ws) = weak.upgrade() {
                    ws.off(sub);
                }
            });
        }
        let grid = cx.root.grid().clone();
        let me = this.me.clone();
        this.bag.on(&grid, "contextmenu", move |ev: JsValue| {
            if let Some(m) = me.upgrade() {
                m.on_context_menu(&ev);
            }
        });
        this
    }

    /// Track the crosshair of every live cell, forget the destroyed ones.
    fn sync_cells(&self) {
        let Some(ws) = self.ws.upgrade() else { return };
        let live: Vec<_> = ws.cells();
        let live_ids: Vec<String> = live.iter().map(|c| c.id().to_string()).collect();
        let gone: Vec<String> = self
            .cells
            .borrow()
            .keys()
            .filter(|k| !live_ids.contains(k))
            .cloned()
            .collect();
        for id in gone {
            self.cells.borrow_mut().remove(&id);
            if let Some(off) = self.subs.borrow_mut().remove(&id) {
                off();
            }
        }
        for cell in live {
            let id = cell.id().to_string();
            if self.cells.borrow().contains_key(&id) {
                continue;
            }
            let Some(chart) = cell.try_chart() else {
                continue;
            };
            let st = Rc::new(RefCell::new(CellState {
                state: ContextMenuState::new(),
            }));
            let st2 = st.clone();
            let off = chart.renderer().on_crosshair_move(Box::new(move |e| {
                st2.borrow_mut().state.on_crosshair(
                    e.price,
                    e.time.map(|t| t as f64),
                    e.pane_kind.map(|k| match k {
                        ModelPaneKind::Price => PointerPaneKind::Price,
                        _ => PointerPaneKind::Study,
                    }),
                    e.pane_id.clone(),
                );
            }));
            self.cells.borrow_mut().insert(id.clone(), st);
            self.subs.borrow_mut().insert(id, off);
        }
    }

    fn flag(chart: &vela_core::vela::Vela, feature: &str) -> bool {
        chart.renderer().get(feature).is_some_and(|v| match v {
            Value::Null => false,
            Value::Bool(b) => b,
            Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
            Value::String(s) => !s.is_empty(),
            _ => true,
        })
    }

    pub fn pane_scales_of(chart: &vela_core::vela::Vela) -> Option<Vec<PaneScaleInfo>> {
        let list = chart.renderer().get("paneScales")?;
        let arr = list.as_array()?;
        Some(
            arr.iter()
                .filter_map(|p| {
                    Some(PaneScaleInfo {
                        id: p.get("id")?.as_str()?.to_string(),
                        kind: if p.get("kind")?.as_str()? == "price" {
                            PaneKind::Price
                        } else {
                            PaneKind::Study
                        },
                        top: p.get("top")?.as_f64()?,
                        height: p.get("height")?.as_f64()?,
                        mode: p.get("mode")?.as_str()?.to_string(),
                        log: p.get("log")?.as_bool()?,
                        invert: p.get("invert")?.as_bool()?,
                    })
                })
                .collect(),
        )
    }

    fn on_context_menu(&self, ev: &JsValue) {
        // An inner menu (or a guard) already answered this right-click.
        if get_bool(ev, "defaultPrevented") {
            return;
        }
        let Some(ws) = self.ws.upgrade() else { return };
        let Some(target) = event_target(ev) else {
            return;
        };
        let Some(cell_el) = closest(&target, |e| {
            has_class(e, "vela-cell") && data(e, "cell-id").is_some()
        }) else {
            return;
        };
        let cell_id = data(&cell_el, "cell-id").unwrap_or_default();
        let (Some(cell), Some(state)) = (
            ws.cell(&cell_id),
            self.cells.borrow().get(&cell_id).cloned(),
        ) else {
            return;
        };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let (mut x, mut y) = (ev_f64(ev, "clientX"), ev_f64(ev, "clientY"));
        let rect = cell_el.get_bounding_client_rect();
        // The context-menu key reports no pointer: open under the focused element.
        if x == 0.0 && y == 0.0 {
            x = rect.left();
            y = rect.bottom();
        }
        let zone = zone_of(x, y, rect.left(), rect.top(), rect.width(), rect.height());
        let panes = Self::pane_scales_of(&chart);
        state
            .borrow_mut()
            .state
            .open(zone, panes.as_deref(), y - rect.top());
        let lang = ws.i18n();
        let pane = state.borrow().state.pane().cloned();
        let builtins = match zone {
            Zone::PriceAxis => {
                let renderer = chart.renderer();
                let mode = renderer
                    .get("scaleMode")
                    .and_then(|v| v.as_str().map(str::to_string));
                let (mode, log) = match &pane {
                    Some(p) => (Some(p.mode.clone()), p.log),
                    None => (mode, Self::flag(&chart, "logScale")),
                };
                price_axis_items(
                    &PriceAxisState {
                        auto: renderer.get("autoScale") != Some(json!(false)),
                        price_only: price_only_state(
                            pane.as_ref(),
                            Self::flag(&chart, "scalePriceOnly"),
                        ),
                        invert: pane
                            .as_ref()
                            .map_or_else(|| Self::flag(&chart, "invertScale"), |p| p.invert),
                        choice: scale_choice_of(mode.as_deref(), log),
                        axis_labels: Self::flag(&chart, "axisLabels"),
                        price_label: Self::flag(&chart, "priceLabel"),
                        countdown: Self::flag(&chart, "countdown"),
                        price_line: Self::flag(&chart, "currentPriceLine"),
                    },
                    Some(&*lang),
                )
            }
            Zone::TimeAxis => time_axis_items(
                &ws.timezone(),
                Some(&*lang),
                self.cx.root.platform().now_ms(),
            ),
            Zone::Body => body_items(
                if chart.drawings().supported() {
                    chart.drawings().all().len()
                } else {
                    0
                },
                chart.indicators().len(),
                Some(&*lang),
            ),
        };
        let pointer = state.borrow().state.pointer().clone();
        let ctx = Rc::new((self.context)().with_pointer(pointer));
        let iconic = builtins.iter().any(|i| i.icon.is_some());
        let rows = self
            .contributions
            .as_ref()
            .map(|c| contributed_rows(c, &*ctx, zone, iconic, Some(&*lang)))
            .unwrap_or_default();
        let items = compose_menu(zone, &builtins, &rows);
        if items.is_empty() {
            return;
        }
        prevent_default(ev);
        let taken = self.menu.borrow_mut().take();
        if let Some(m) = taken {
            m.close(false);
        }
        *self.open.borrow_mut() = Some((cell_id, zone, ctx));
        let me = self.me.clone();
        let handle = open_menu(
            &self.cx,
            &items,
            x,
            y,
            "",
            None,
            Rc::new(move |id: &str| {
                if let Some(m) = me.upgrade() {
                    m.run(id);
                }
            }),
            None,
        );
        *self.menu.borrow_mut() = handle;
    }

    /// The open menu (tests and hosts read it).
    pub fn open_menu_element(&self) -> Option<Element> {
        self.menu
            .borrow()
            .as_ref()
            .filter(|m| m.is_open())
            .map(|m| m.element().clone())
    }

    fn run(&self, id: &str) {
        let Some((cell_id, zone, ctx)) = self.open.borrow().clone() else {
            return;
        };
        let Some(ws) = self.ws.upgrade() else { return };
        let Some(cell) = ws.cell(&cell_id) else {
            return;
        };
        let Some(chart) = cell.try_chart() else {
            return;
        };
        let Some(cmd) = route_selection(id) else {
            return;
        };
        let pane = self
            .cells
            .borrow()
            .get(&cell_id)
            .and_then(|s| s.borrow().state.pane().cloned());
        let renderer = chart.renderer();
        match cmd {
            MenuCommand::RunAction(action) => {
                if let Some(c) = &self.contributions
                    && let Some(a) = c
                        .widget_actions(target_of(zone), Some(&*ctx as &dyn WidgetContext))
                        .into_iter()
                        .find(|a| a.id == action)
                {
                    (a.run)(&*ctx);
                }
            }
            MenuCommand::ToggleFeature(feature) => {
                let on = Self::flag(&chart, &feature);
                renderer.set(&feature, json!(!on));
            }
            MenuCommand::Scale(choice) => {
                for (feature, value) in scale_writes(choice, pane.as_ref()) {
                    renderer.set(&feature, value);
                }
            }
            MenuCommand::Timezone(zone) => ws.set_timezone(&zone),
            MenuCommand::AutoScale => {
                let off = renderer.get("autoScale") == Some(json!(false));
                renderer.set("autoScale", json!(off));
            }
            MenuCommand::PriceOnly => {
                let on = Self::flag(&chart, "scalePriceOnly");
                renderer.set("scalePriceOnly", json!(!on));
            }
            MenuCommand::Invert => {
                let cur = pane
                    .as_ref()
                    .map_or_else(|| Self::flag(&chart, "invertScale"), |p| p.invert);
                let (feature, value) = invert_write(!cur, pane.as_ref());
                renderer.set(&feature, value);
            }
            MenuCommand::Settings(section) => {
                renderer.open_settings(section.as_deref());
            }
            MenuCommand::ResetView => cell.reset_view(),
            MenuCommand::RemoveDrawings => {
                if chart.drawings().supported() {
                    for d in chart.drawings().all() {
                        chart.drawings().remove(&d.id);
                    }
                }
            }
            MenuCommand::RemoveIndicators => {
                for h in chart.indicators() {
                    h.remove();
                }
            }
        }
        let _ = ScaleChoice::Regular;
    }

    pub fn destroy(&self) {
        let taken = self.menu.borrow_mut().take();
        if let Some(m) = taken {
            m.close(false);
        }
        self.bag.clear();
        let subs: Vec<_> = self.subs.borrow_mut().drain().map(|(_, f)| f).collect();
        for f in subs {
            f();
        }
        self.cells.borrow_mut().clear();
    }
}
