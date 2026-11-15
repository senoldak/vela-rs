//! The object tree (`object-tree.ts`): a docked side panel listing everything on the chart,
//! grouped by pane. Each pane block names itself and carries its ops (move, collapse,
//! maximize); inside it sits the pane's whole stack (drawings, indicators and, in the main
//! pane, the candles) as ONE column ordered front-first. Rows carry eye / lock / remove
//! actions, drawing rows select on the chart (and the chart's selection comes back as the
//! pick), rows drag to restack or re-pane with a ghost and drop hints, rows have menus, groups
//! bundle drawings.
//!
//! The layout and every rule are `vela_core::widget::{object_tree, object_tree_model}`: the
//! snapshot is taken here, [`build_tree`] lays it out, the panel's own state is an
//! [`ObjectTreeModel`] (one per cell, since each chart keeps its own bundles), a drag is a
//! [`DragSession`] resolved by [`resolve_drop`] from the measured geometry and applied from a
//! [`DropPlan`], a menu row is a [`TreeCommand`]. This file is the element tree, the pointer
//! plumbing and the chart calls.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use serde_json::json;
use vela_core::drawings::controller::DrawingUpdate;
use vela_core::drawings::drawing::SerializedDrawing;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::model::PaneKind;
use vela_core::ports::chart_renderer::{MoveTarget, NewPanePlacement, PaneMoveDir};
use vela_core::vela::Vela;
use vela_core::widget::object_tree::{
    DragKind, DragSession, Drop as DropAt, DropPlan, GapGeometry, IndicatorTarget, LiveDrawing,
    MenuBuilder, MenuInputs, ObjectTreeModel, PaneGeometry, PassFlags, REFRESH_EVENTS, Rect,
    StackPlan, TreeCommand, TreeText, UnitGeometry, drawing_menu, find_indicator_row,
    group_drawings_label, group_members_label, group_menu, indicator_menu, object_tree_title,
    plan_drop, plan_restack_group, price_menu, resolve_drop, selected_label,
};
use vela_core::widget::object_tree_model::{
    DrawGroup, DrawUnit, PRICE_PANE_ID, TreeItem, TreePane, TreeRow, TreeSnapshot, ZEntry,
    build_tree, can_group, drawing_label, drawing_meta, group_state, tree_is_empty,
};
use vela_core::widget::side_panel::SidePanelOptions;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use crate::dom::{attr, default_view, prevent_default, set_style, stop_propagation};

use super::bind::{ChartSubs, chart_on};
use super::dock::{Dock, PanelSpec, PanelView};
use super::kit::{
    Bag, Cx, clear, closest, data, ev_f64, ev_flag, event_target, find_all, get_value, has_class,
    icon_el, is_hidden, kids, markup_icon, set_data, set_disabled, set_title, set_value,
};
use super::menu::{MenuHandle, open_menu};

const LONG_PRESS_MS: f64 = 500.0;
const PRESS_SLOP: f64 = 10.0;

/// What one render pass produced and the interactions read back.
struct Pass {
    chart: Vela,
    panes: Vec<TreePane>,
    flags: PassFlags,
    drawings: Vec<SerializedDrawing>,
    series_order: Vec<ZEntry>,
    candle_z: f64,
    panes_supported: bool,
    supports_indicator_settings: bool,
}

struct RowAction {
    icon: &'static str,
    title: String,
    engaged: bool,
    disabled: bool,
    run: Rc<dyn Fn()>,
}

/// The measured elements and geometry of one drag move.
/// A pane block's stack: each unit with its member rows.
type UnitEls = Vec<(Element, Vec<Element>)>;

struct Geo {
    gaps: Vec<(Element, GapGeometry)>,
    panes: Vec<(Element, PaneGeometry, UnitEls)>,
}

struct Drag {
    session: DragSession,
    ghost: Option<Element>,
    drop: Option<DropAt>,
    geo: Option<Geo>,
    bag: Rc<Bag>,
}

pub struct ObjectTree {
    cx: Cx,
    me: Weak<ObjectTree>,
    pub view: Rc<PanelView>,
    chart: RefCell<Option<Vela>>,
    cell: RefCell<String>,
    models: RefCell<HashMap<String, ObjectTreeModel>>,
    selected: RefCell<HashSet<String>>,
    syncing: Cell<bool>,
    pass: RefCell<Option<Pass>>,
    bag: Rc<Bag>,
    static_bag: Rc<Bag>,
    subs: ChartSubs,
    drag: RefCell<Option<Drag>>,
    menu: RefCell<Option<Rc<MenuHandle>>>,
    commands: RefCell<HashMap<String, TreeCommand>>,
    press: RefCell<Option<(f64, f64, vela_core::runtime::TimerId)>>,
    press_opened: Cell<bool>,
    symbol_raw: RefCell<String>,
}

impl ObjectTree {
    pub fn new(cx: &Cx, dock: &Rc<Dock>) -> Rc<ObjectTree> {
        let view = dock.add_built_in(PanelSpec {
            id: "objects",
            title: object_tree_title(),
            icon: "objects",
            order: 20.0,
            modifier: "vela-ot",
            opts: SidePanelOptions::default(),
        });
        let tree = Rc::new_cyclic(|me| ObjectTree {
            cx: cx.clone(),
            me: me.clone(),
            view,
            chart: RefCell::new(None),
            cell: RefCell::default(),
            models: RefCell::default(),
            selected: RefCell::default(),
            syncing: Cell::new(false),
            pass: RefCell::new(None),
            bag: Bag::new(),
            static_bag: Bag::new(),
            subs: ChartSubs::default(),
            drag: RefCell::new(None),
            menu: RefCell::new(None),
            commands: RefCell::default(),
            press: RefCell::new(None),
            press_opened: Cell::new(false),
            symbol_raw: RefCell::default(),
        });
        let w = Rc::downgrade(&tree);
        dock.on_open_hook(
            "objects",
            Rc::new(move || {
                if let Some(t) = w.upgrade() {
                    t.refresh();
                }
            }),
        );
        let w = Rc::downgrade(&tree);
        dock.on_chart_hook(
            "objects",
            Rc::new(move |cell: &str, chart: &Vela| {
                if let Some(t) = w.upgrade() {
                    t.on_chart(cell, chart);
                }
            }),
        );
        tree.wire_body();
        tree
    }

    fn tx(&self, key: TreeText) -> String {
        let tr = self.cx.tr();
        key.text(tr.as_deref())
    }

    fn with_model<R>(&self, f: impl FnOnce(&mut ObjectTreeModel) -> R) -> R {
        let id = self.cell.borrow().clone();
        let mut models = self.models.borrow_mut();
        f(models.entry(id).or_default())
    }

    fn model_snapshot(&self) -> ObjectTreeModel {
        self.with_model(|m| m.clone())
    }

    // ── binding ──

    /// (Re)bind to a chart instance.
    pub fn on_chart(&self, cell: &str, chart: &Vela) {
        self.subs.clear();
        *self.cell.borrow_mut() = cell.to_string();
        *self.chart.borrow_mut() = Some(chart.clone());
        self.selected.borrow_mut().clear();
        for ev in REFRESH_EVENTS
            .into_iter()
            .chain([ChartEventName::MarketChanged])
        {
            let me = self.me.clone();
            self.subs.push(chart_on(chart, ev, move |_| {
                if let Some(t) = me.upgrade() {
                    t.refresh();
                }
            }));
        }
        // The stacking rides the renderer config: a reorder made on the chart reaches the
        // tree through it.
        let me = self.me.clone();
        let off = chart.renderer().on_config_changed(Box::new(move || {
            if let Some(t) = me.upgrade() {
                t.refresh();
            }
        }));
        self.subs.push(off);
        // Selection both stores and repaints, in that order.
        let me = self.me.clone();
        self.subs.push(chart_on(
            chart,
            ChartEventName::DrawingSelected,
            move |ev: &ChartEvent| {
                let Some(t) = me.upgrade() else { return };
                if let ChartEvent::DrawingSelected { ids, .. } = ev {
                    *t.selected.borrow_mut() = ids.iter().cloned().collect();
                    // A selection made on the chart takes over the panel's pick, all of it;
                    // our own echo does not.
                    if !t.syncing.get() {
                        t.with_model(|m| m.set_picked(ids));
                    }
                    t.refresh();
                }
            },
        ));
        self.refresh();
    }

    pub fn detach(&self) {
        self.subs.clear();
        *self.chart.borrow_mut() = None;
    }

    pub fn relocalize(&self) {
        self.refresh();
    }

    pub fn destroy(&self) {
        self.detach();
        self.end_drag(false);
        self.close_menu();
        self.cancel_press();
        self.bag.clear();
        self.static_bag.clear();
    }

    fn close_menu(&self) {
        let m = self.menu.borrow_mut().take();
        if let Some(m) = m {
            m.close(false);
        }
    }

    /// Repaint because something changed, unless the panel is mid-interaction.
    pub fn refresh(&self) {
        if self
            .drag
            .borrow()
            .as_ref()
            .is_some_and(|d| d.session.active())
        {
            return;
        }
        if self.with_model(|m| m.renaming().is_some()) {
            return;
        }
        self.render();
    }

    // ── snapshot ──

    fn series_order(chart: &Vela) -> Vec<ZEntry> {
        chart
            .renderer()
            .get("seriesOrder")
            .and_then(|v| v.as_array().cloned())
            .map(|list| {
                list.iter()
                    .filter_map(|e| {
                        Some(ZEntry {
                            id: e.get("id")?.as_str()?.to_string(),
                            z: e.get("z")?.as_f64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn symbol_name(&self) -> String {
        let cell = self.cell.borrow().clone();
        let raw = self
            .cx
            .root
            .workspace()
            .and_then(|ws| ws.cell(&cell))
            .map(|c| c.symbol())
            .unwrap_or_default();
        *self.symbol_raw.borrow_mut() = raw.clone();
        vela_core::data::provider_registry::parse_symbol(&raw).ticker
    }

    fn prune(&self, chart: &Vela) {
        let live: HashSet<String> = if chart.drawings().supported() {
            chart.drawings().all().into_iter().map(|d| d.id).collect()
        } else {
            HashSet::new()
        };
        self.with_model(|m| m.prune(&live));
    }

    fn render(&self) {
        if is_hidden(&self.view.el) {
            return;
        }
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        self.prune(&chart);
        let model = self.model_snapshot();
        let handles = chart.indicators();
        let visible = |id: &str| {
            handles
                .iter()
                .find(|h| h.id() == id)
                .is_none_or(|h| h.visible())
        };
        let title = |id: &str| handles.iter().find(|h| h.id() == id).map(|h| h.title());
        let renderer = chart.renderer();
        let stackable = renderer.supports("seriesOrder") && renderer.supports("candleZOrder");
        let drawings_supported = chart.drawings().supported();
        let interleave = stackable && drawings_supported && renderer.capabilities().drawing_depth;
        let series_order = if stackable {
            Self::series_order(&chart)
        } else {
            Vec::new()
        };
        let candle_z = if stackable {
            renderer
                .get("candleZOrder")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let drawings = if drawings_supported {
            chart.drawings().all()
        } else {
            Vec::new()
        };
        let symbol = self.symbol_name();
        let tr = self.cx.tr();
        let price_label = if symbol.is_empty() {
            self.tx(TreeText::Price)
        } else {
            symbol.clone()
        };
        let pane_infos = chart.panes().list();
        let registry = self.cx.root.env().registries.drawings.clone();
        let panes = {
            let snap = TreeSnapshot {
                panes: &pane_infos,
                i18n: tr.as_deref(),
                registry: Some(&registry),
                indicator_visible: &visible,
                handle_title: &title,
                stackable,
                interleave,
                z_order: &series_order,
                candle_z,
                price_label,
                price_visible: renderer.get("candleVisible").and_then(|v| v.as_bool())
                    != Some(false),
                drawings: &drawings,
                groups: model.groups(),
            };
            build_tree(&snap)
        };
        let flags = PassFlags {
            stackable,
            repanable: chart.panes().supported(),
            interleave,
        };
        *self.pass.borrow_mut() = Some(Pass {
            chart: chart.clone(),
            panes: panes.clone(),
            flags,
            drawings: drawings.clone(),
            series_order,
            candle_z,
            panes_supported: chart.panes().supported(),
            supports_indicator_settings: renderer.supports_indicator_settings(),
        });

        // Paint.
        self.bag.retire(&self.cx);
        let body = &self.view.body;
        clear(body);
        let _ = body.append_child(&self.selection_bar(&chart, &model, drawings_supported));
        for (i, pane) in panes.iter().enumerate() {
            if i > 0 {
                let _ =
                    body.append_child(&self.gap_el(flags, Some(&panes[i - 1].id), Some(&pane.id)));
            }
            let _ =
                body.append_child(&self.pane_block(&chart, &model, &drawings, flags, &panes, pane));
        }
        if let Some(last) = panes.last()
            && flags.repanable
        {
            let _ = body.append_child(&self.gap_el(flags, Some(&last.id), None));
        }
        if tree_is_empty(&panes) {
            let _ = body.append_child(&self.cx.text(
                "div",
                "vela-ot-empty",
                &self.tx(TreeText::EmptyHint),
            ));
        }
    }

    fn selection_bar(
        &self,
        chart: &Vela,
        model: &ObjectTreeModel,
        drawings_supported: bool,
    ) -> Element {
        let _ = chart;
        let bar = self.cx.el("div", "vela-ot-selbar");
        let ids: Vec<String> = model.picked().to_vec();
        let groupable = can_group(&ids, model.groups());
        let tr = self.cx.tr();
        let me = self.me.clone();
        let title = if ids.is_empty() {
            self.tx(TreeText::NewGroup)
        } else if groupable {
            group_drawings_label(ids.len(), tr.as_deref())
        } else {
            self.tx(TreeText::AlreadyInAGroup)
        };
        let g_ids = ids.clone();
        let _ = bar.append_child(&self.btn(RowAction {
            icon: "group",
            title,
            engaged: false,
            disabled: !drawings_supported || (!ids.is_empty() && !groupable),
            run: Rc::new(move || {
                if let Some(t) = me.upgrade() {
                    if g_ids.is_empty() {
                        t.make_empty_group();
                    } else {
                        t.make_group(&g_ids);
                    }
                }
            }),
        }));
        let me = self.me.clone();
        let d_ids = ids.clone();
        let _ = bar.append_child(&self.btn(RowAction {
            icon: "clone",
            title: self.tx(if ids.is_empty() {
                TreeText::DuplicateSelected
            } else {
                TreeText::Duplicate
            }),
            engaged: false,
            disabled: ids.is_empty(),
            run: Rc::new(move || {
                if let Some(t) = me.upgrade() {
                    for id in &d_ids {
                        t.clone_drawing_into(id);
                    }
                    t.render();
                }
            }),
        }));
        let count = self.cx.text(
            "span",
            "vela-ot-selcount",
            &if ids.len() > 1 {
                selected_label(ids.len(), tr.as_deref())
            } else {
                String::new()
            },
        );
        let _ = bar.append_child(&count);
        bar
    }

    fn gap_el(&self, flags: PassFlags, after: Option<&str>, before: Option<&str>) -> Element {
        if !flags.repanable {
            return self.cx.el("div", "vela-ot-panesep");
        }
        let el = self.cx.el("div", "vela-ot-gap");
        if let Some(b) = before {
            set_data(&el, "before", Some(b));
        }
        if let Some(a) = after {
            set_data(&el, "after", Some(a));
        }
        el
    }

    fn pane_block(
        &self,
        chart: &Vela,
        model: &ObjectTreeModel,
        drawings: &[SerializedDrawing],
        flags: PassFlags,
        all: &[TreePane],
        pane: &TreePane,
    ) -> Element {
        let block = self.cx.el("div", "vela-ot-pane");
        set_data(&block, "pane", Some(&pane.id));
        set_data(
            &block,
            "kind",
            Some(if pane.kind == PaneKind::Price {
                "price"
            } else {
                "study"
            }),
        );
        let _ = block.append_child(&self.pane_head(chart, all, pane));
        let stack = self.cx.el("div", "vela-ot-stack");
        for item in &pane.items {
            let el = match item {
                TreeItem::Row(row) => self.row_el(chart, flags, pane, row),
                TreeItem::Unit(unit) => self.unit_el(chart, model, drawings, pane, unit),
            };
            let _ = stack.append_child(&el);
        }
        let _ = block.append_child(&stack);
        block
    }

    fn unit_el(
        &self,
        chart: &Vela,
        model: &ObjectTreeModel,
        all_drawings: &[SerializedDrawing],
        pane: &TreePane,
        unit: &DrawUnit,
    ) -> Element {
        let wrap = self.cx.el("div", "vela-ot-unit");
        match unit {
            DrawUnit::Draw(d) => {
                let _ = wrap.append_child(&self.draw_row(chart, model, d, false));
            }
            DrawUnit::Group { group, members } => {
                set_data(&wrap, "group", Some(&group.id));
                let _ = wrap.append_child(&self.group_row(chart, model, all_drawings, pane, group));
                if !model.is_collapsed(&group.id) {
                    for m in members {
                        let _ = wrap.append_child(&self.draw_row(chart, model, m, true));
                    }
                    if members.is_empty() {
                        let _ = wrap.append_child(&self.cx.text(
                            "div",
                            "vela-ot-grouphint",
                            &self.tx(TreeText::GroupDropHint),
                        ));
                    }
                }
            }
        }
        wrap
    }

    fn pane_head(&self, chart: &Vela, all: &[TreePane], pane: &TreePane) -> Element {
        let head = self.cx.el("div", "vela-ot-panehead");
        let name = self.cx.text("span", "vela-ot-panename", &pane.label);
        set_title(&name, &pane.label);
        let _ = head.append_child(&name);
        if !chart.panes().supported() {
            return head;
        }
        let acts = self.cx.el("span", "vela-ot-acts");
        let _ = head.append_child(&acts);
        let op = |icon: &'static str, title: String, run: Rc<dyn Fn()>| {
            let _ = acts.append_child(&self.btn(RowAction {
                icon,
                title,
                engaged: false,
                disabled: false,
                run,
            }));
        };
        let id = pane.id.clone();
        if pane.kind != PaneKind::Price {
            if pane.order > 1 {
                let (c, id) = (chart.clone(), id.clone());
                op(
                    "arrow-up",
                    self.tx(TreeText::MovePaneUp),
                    Rc::new(move || {
                        c.panes().move_pane(&id, PaneMoveDir::Up);
                    }),
                );
            }
            if pane.order + 1 < all.len() {
                let (c, id) = (chart.clone(), id.clone());
                op(
                    "arrow-down",
                    self.tx(TreeText::MovePaneDown),
                    Rc::new(move || {
                        c.panes().move_pane(&id, PaneMoveDir::Down);
                    }),
                );
            }
            let (c, id, collapsed) = (chart.clone(), id.clone(), pane.collapsed);
            op(
                if collapsed { "expand" } else { "collapse" },
                self.tx(if collapsed {
                    TreeText::ExpandPane
                } else {
                    TreeText::CollapsePane
                }),
                Rc::new(move || {
                    c.panes().collapse(&id, !collapsed);
                }),
            );
        }
        let (c, id, maxed) = (chart.clone(), id, pane.maximized);
        op(
            if maxed { "restore" } else { "maximize" },
            self.tx(if maxed {
                TreeText::RestorePanes
            } else {
                TreeText::MaximizePane
            }),
            Rc::new(move || {
                c.panes().maximize(if maxed { None } else { Some(&id) });
            }),
        );
        head
    }

    fn row_el(&self, chart: &Vela, flags: PassFlags, pane: &TreePane, row: &TreeRow) -> Element {
        let me = self.me.clone();
        match row {
            TreeRow::Price(p) => {
                let symbol = self.symbol_name();
                let base = strip_quote(&symbol);
                let name = if symbol.is_empty() {
                    self.tx(TreeText::Price)
                } else {
                    symbol.clone()
                };
                let fallback = vela_core::i18n::slice_graphemes(
                    &*self.cx.root.env().intl,
                    &self.tx(TreeText::Price),
                    1,
                );
                let raw = self.symbol_raw.borrow().clone();
                let url = if raw.is_empty() {
                    None
                } else {
                    self.cx
                        .root
                        .workspace()
                        .and_then(|ws| ws.feed().symbol_icon(&raw))
                };
                let icon = ticker_icon_el(
                    &self.cx,
                    if base.is_empty() { &fallback } else { &base },
                    &name,
                    "vela-ot-avatar",
                    url.as_deref(),
                );
                let visible = p.visible;
                let c = chart.clone();
                let el = self.row(
                    icon,
                    &p.label,
                    p.visible,
                    vec![RowAction {
                        icon: if visible { "eye" } else { "eye-off" },
                        title: self.tx(if visible {
                            TreeText::Hide
                        } else {
                            TreeText::Show
                        }),
                        engaged: !visible,
                        disabled: false,
                        run: Rc::new(move || {
                            c.renderer().set("candleVisible", json!(!visible));
                            if let Some(t) = me.upgrade() {
                                t.refresh();
                            }
                        }),
                    }],
                    None,
                );
                set_data(&el, "row-kind", Some("price"));
                set_data(&el, "pane", Some(&pane.id));
                if flags.stackable {
                    set_data(&el, "drag", Some("1"));
                    set_title(&el, &self.tx(TreeText::RestackHint));
                }
                el
            }
            TreeRow::Indicator(i) => {
                let id = i.id.clone();
                let visible = i.visible;
                let c = chart.clone();
                let (id1, me1) = (id.clone(), me.clone());
                let (id2, c2, me2) = (id.clone(), c.clone(), me.clone());
                let el = self.row(
                    icon_el(&self.cx.doc, "indicators"),
                    &i.label,
                    visible,
                    vec![
                        RowAction {
                            icon: if visible { "eye" } else { "eye-off" },
                            title: self.tx(if visible {
                                TreeText::Hide
                            } else {
                                TreeText::Show
                            }),
                            engaged: !visible,
                            disabled: false,
                            run: Rc::new(move || {
                                if let Some(h) = c.indicators().into_iter().find(|h| h.id() == id1)
                                {
                                    h.set_visible(!visible);
                                }
                                if let Some(t) = me1.upgrade() {
                                    t.refresh();
                                }
                            }),
                        },
                        RowAction {
                            icon: "trash",
                            title: self.tx(TreeText::Remove),
                            engaged: false,
                            disabled: false,
                            run: Rc::new(move || {
                                if let Some(h) = c2.indicators().into_iter().find(|h| h.id() == id2)
                                {
                                    h.remove();
                                }
                                if let Some(t) = me2.upgrade() {
                                    t.refresh();
                                }
                            }),
                        },
                    ],
                    i.own_scale.then(|| self.tx(TreeText::ScaleTag)),
                );
                set_data(&el, "row-kind", Some("indicator"));
                set_data(&el, "id", Some(&id));
                set_data(&el, "pane", Some(&pane.id));
                if flags.repanable || flags.stackable {
                    set_data(&el, "drag", Some("1"));
                    set_title(
                        &el,
                        &self.tx(if flags.repanable {
                            TreeText::MoveIndicatorHint
                        } else {
                            TreeText::RestackHint
                        }),
                    );
                }
                el
            }
        }
    }

    fn group_row(
        &self,
        chart: &Vela,
        model: &ObjectTreeModel,
        all_drawings: &[SerializedDrawing],
        pane: &TreePane,
        g: &DrawGroup,
    ) -> Element {
        let folded = model.is_collapsed(&g.id);
        let members: Vec<SerializedDrawing> = all_drawings
            .iter()
            .filter(|d| g.ids.contains(&d.id))
            .cloned()
            .collect();
        let (all_hidden, all_locked) = group_state(&members);
        let ids = g.ids.clone();
        let mk = |icon: &'static str, title: TreeText, engaged: bool, f: Rc<dyn Fn()>| RowAction {
            icon,
            title: self.tx(title),
            engaged,
            disabled: false,
            run: f,
        };
        let me = self.me.clone();
        let actions = if g.ids.is_empty() {
            let (me, gid) = (me.clone(), g.id.clone());
            vec![mk(
                "trash",
                TreeText::Remove,
                false,
                Rc::new(move || {
                    if let Some(t) = me.upgrade() {
                        t.remove_group(&gid, false);
                    }
                }),
            )]
        } else {
            let (m1, i1) = (me.clone(), ids.clone());
            let (m2, i2) = (me.clone(), ids.clone());
            let (m3, gid) = (me.clone(), g.id.clone());
            vec![
                mk(
                    if all_locked { "lock" } else { "unlock" },
                    if all_locked {
                        TreeText::UnlockAll
                    } else {
                        TreeText::LockAll
                    },
                    all_locked,
                    Rc::new(move || {
                        if let Some(t) = m1.upgrade() {
                            t.set_members(
                                &i1,
                                DrawingUpdate {
                                    locked: Some(!all_locked),
                                    ..Default::default()
                                },
                            );
                        }
                    }),
                ),
                mk(
                    if all_hidden { "eye-off" } else { "eye" },
                    if all_hidden {
                        TreeText::ShowAll
                    } else {
                        TreeText::HideAll
                    },
                    all_hidden,
                    Rc::new(move || {
                        if let Some(t) = m2.upgrade() {
                            t.set_members(
                                &i2,
                                DrawingUpdate {
                                    visible: Some(all_hidden),
                                    ..Default::default()
                                },
                            );
                        }
                    }),
                ),
                mk(
                    "trash",
                    TreeText::RemoveAll,
                    false,
                    Rc::new(move || {
                        if let Some(t) = m3.upgrade() {
                            t.remove_group(&gid, true);
                        }
                    }),
                ),
            ]
        };
        let _ = chart;
        let el = self.row(
            icon_el(
                &self.cx.doc,
                if folded {
                    "chevron-right"
                } else {
                    "chevron-down"
                },
            ),
            &g.name,
            !all_hidden,
            actions,
            None,
        );
        set_data(&el, "row-kind", Some("group"));
        set_data(&el, "id", Some(&g.id));
        set_data(&el, "pane", Some(&pane.id));
        let tr = self.cx.tr();
        set_title(
            &el,
            &if g.ids.is_empty() {
                self.tx(TreeText::GroupDropHint)
            } else {
                group_members_label(members.len(), tr.as_deref())
            },
        );
        if model.renaming() == Some(g.id.as_str()) {
            // A row being renamed is a text field, not a handle.
            let input = self.cx.el("input", "vela-ot-rename");
            set_value(&input, &g.name);
            if let Some(name) = kids(&el).into_iter().find(|k| has_class(k, "vela-ot-name")) {
                let _ = el.insert_before(&input, Some(&name));
                name.remove();
            }
            let inp = input.clone();
            self.cx.root.defer(move || {
                super::kit::focus(&inp);
                if let Ok(f) = js_sys::Reflect::get(&inp, &JsValue::from_str("select"))
                    && let Some(f) = f.dyn_ref::<js_sys::Function>()
                {
                    let _ = f.call0(&inp);
                }
            });
            let (me, gid, inp) = (self.me.clone(), g.id.clone(), input.clone());
            self.bag.on(&input, "keydown", move |ev: JsValue| {
                let key = super::kit::ev_key(&ev);
                let Some(t) = me.upgrade() else { return };
                if key == "Enter" {
                    prevent_default(&ev);
                    t.commit_rename(&gid, &get_value(&inp));
                } else if key == "Escape" {
                    t.with_model(|m| {
                        // Cancel: end the rename keeping the name.
                        m.commit_rename(&gid, "");
                    });
                    t.render();
                } else if key == " " {
                    stop_propagation(&ev);
                }
            });
            let (me, gid, inp) = (self.me.clone(), g.id.clone(), input.clone());
            self.bag.on(&input, "blur", move |_| {
                if let Some(t) = me.upgrade() {
                    t.commit_rename(&gid, &get_value(&inp));
                }
            });
        } else {
            if !g.ids.is_empty() {
                set_data(&el, "drag", Some("1"));
            }
            let (me, gid) = (self.me.clone(), g.id.clone());
            self.bag.on(&el, "click", move |_| {
                if let Some(t) = me.upgrade() {
                    t.with_model(|m| m.toggle_collapsed(&gid));
                    t.render();
                }
            });
        }
        el
    }

    fn draw_row(
        &self,
        chart: &Vela,
        model: &ObjectTreeModel,
        d: &SerializedDrawing,
        in_group: bool,
    ) -> Element {
        let tr = self.cx.tr();
        let registry = self.cx.root.env().registries.drawings.clone();
        let (_, markup) = drawing_meta(&d.type_key, tr.as_deref(), Some(&registry));
        let icon = match markup {
            Some(m) if !m.is_empty() => markup_icon(&self.cx.doc, &m),
            _ => icon_el(&self.cx.doc, "pen"),
        };
        let label = drawing_label(d, tr.as_deref(), Some(&registry));
        let (id, locked, visible) = (d.id.clone(), d.locked, d.visible);
        let mk = |icon: &'static str, title: TreeText, engaged: bool, f: Rc<dyn Fn()>| RowAction {
            icon,
            title: self.tx(title),
            engaged,
            disabled: false,
            run: f,
        };
        let (c1, i1, m1) = (chart.clone(), id.clone(), self.me.clone());
        let (c2, i2, m2) = (chart.clone(), id.clone(), self.me.clone());
        let (c3, i3, m3) = (chart.clone(), id.clone(), self.me.clone());
        let el = self.row(
            icon,
            &label,
            visible,
            vec![
                mk(
                    if locked { "lock" } else { "unlock" },
                    if locked {
                        TreeText::Unlock
                    } else {
                        TreeText::Lock
                    },
                    locked,
                    Rc::new(move || {
                        c1.drawings().lock(&i1, !locked);
                        if let Some(t) = m1.upgrade() {
                            t.refresh();
                        }
                    }),
                ),
                mk(
                    if visible { "eye" } else { "eye-off" },
                    if visible {
                        TreeText::Hide
                    } else {
                        TreeText::Show
                    },
                    !visible,
                    Rc::new(move || {
                        c2.drawings().show(&i2, !visible);
                        if let Some(t) = m2.upgrade() {
                            t.refresh();
                        }
                    }),
                ),
                mk(
                    "trash",
                    TreeText::Remove,
                    false,
                    Rc::new(move || {
                        c3.drawings().remove(&i3);
                        if let Some(t) = m3.upgrade() {
                            t.refresh();
                        }
                    }),
                ),
            ],
            None,
        );
        set_data(&el, "row-kind", Some("drawing"));
        set_data(&el, "id", Some(&id));
        set_data(&el, "pane", Some(&d.pane_id));
        set_data(&el, "drag", Some("1"));
        set_title(&el, &self.tx(TreeText::MoveDrawingHint));
        if in_group {
            super::kit::add_class(&el, "vela-ot-subrow");
        }
        if self.selected.borrow().contains(&d.id) {
            set_data(&el, "selected", Some("1"));
        }
        if model.is_picked(&d.id) {
            set_data(&el, "picked", Some("1"));
        }
        let (me, id) = (self.me.clone(), id);
        self.bag.on(&el, "click", move |ev: JsValue| {
            if let Some(t) = me.upgrade() {
                t.on_draw_click(&id, ev_flag(&ev, "ctrlKey") || ev_flag(&ev, "metaKey"));
            }
        });
        el
    }

    /// Clicking a drawing picks it; the platform's modifier extends the pick, and clicking
    /// the only picked row clears it. The chart mirrors whatever comes out.
    fn on_draw_click(&self, id: &str, extend: bool) {
        let picked = self.with_model(|m| m.on_draw_click(id, extend));
        self.select_on_chart(&picked);
        self.render();
    }

    fn select_on_chart(&self, ids: &[String]) {
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        if !chart.drawings().supported() {
            return;
        }
        self.syncing.set(true);
        chart.drawings().select(ids);
        self.syncing.set(false);
    }

    fn row(
        &self,
        icon: Element,
        label: &str,
        visible: bool,
        actions: Vec<RowAction>,
        tag: Option<String>,
    ) -> Element {
        let el = self.cx.el("div", "vela-ot-row");
        if !visible {
            set_data(&el, "hidden", Some("1"));
        }
        let name = self.cx.text("span", "vela-ot-name", label);
        set_title(&name, label);
        let _ = el.append_child(&icon);
        let _ = el.append_child(&name);
        if let Some(tag) = tag {
            let t = self.cx.text("span", "vela-ot-tag", &tag);
            set_title(&t, &self.tx(TreeText::OwnScaleHint));
            let _ = el.append_child(&t);
        }
        let acts = self.cx.el("span", "vela-ot-acts");
        for a in actions {
            let _ = acts.append_child(&self.btn(a));
        }
        let _ = el.append_child(&acts);
        el
    }

    fn btn(&self, a: RowAction) -> Element {
        let b = self.cx.el("button", "vela-ot-btn");
        attr(&b, "type", "button");
        set_data(&b, "icon", Some(a.icon));
        if a.engaged {
            set_data(&b, "engaged", Some("1"));
        }
        if a.disabled {
            set_disabled(&b, true);
        }
        set_title(&b, &a.title);
        attr(&b, "aria-label", &a.title);
        let _ = b.append_child(&icon_el(&self.cx.doc, a.icon));
        let (run, disabled) = (a.run.clone(), a.disabled);
        self.bag.on(&b, "click", move |ev: JsValue| {
            stop_propagation(&ev);
            if !disabled {
                run();
            }
        });
        b
    }

    // ── groups ──

    fn make_group(&self, ids: &[String]) {
        let tr = self.cx.tr();
        self.with_model(|m| m.make_group(ids, tr.as_deref()));
        self.render();
    }

    fn make_empty_group(&self) {
        let tr = self.cx.tr();
        self.with_model(|m| m.make_empty_group(tr.as_deref()));
        self.render();
    }

    fn commit_rename(&self, id: &str, value: &str) {
        if self.with_model(|m| m.commit_rename(id, value)) {
            self.render();
        }
    }

    fn remove_group(&self, id: &str, with_members: bool) {
        let ids = self.with_model(|m| m.remove_group(id, with_members));
        if with_members
            && !ids.is_empty()
            && let Some(chart) = self.chart.borrow().clone()
        {
            chart.drawings().remove_many(&ids);
        }
        self.render();
    }

    fn set_members(&self, ids: &[String], patch: DrawingUpdate) {
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        chart
            .drawings()
            .update_many(ids.iter().map(|id| (id.clone(), patch.clone())).collect());
        self.render();
    }

    /// Duplicate a drawing, keeping the copy in the same group as its original.
    fn clone_drawing_into(&self, id: &str) {
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        if !chart.drawings().supported() {
            return;
        }
        let before: HashSet<String> = chart.drawings().all().into_iter().map(|d| d.id).collect();
        chart.drawings().clone_drawing(id);
        if let Some(clone) = chart
            .drawings()
            .all()
            .into_iter()
            .find(|d| !before.contains(&d.id))
        {
            self.with_model(|m| m.file_clone(id, &clone.id));
        }
    }

    // ── commands (menu rows) ──

    fn run_command(&self, cmd: TreeCommand) {
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        let indicator = |id: &str| chart.indicators().into_iter().find(|h| h.id() == id);
        match cmd {
            TreeCommand::SetCandlesVisible(v) => {
                chart.renderer().set("candleVisible", json!(v));
            }
            TreeCommand::SetCandleZ(z) => {
                chart.renderer().set("candleZOrder", json!(z));
            }
            TreeCommand::SetIndicatorVisible { id, visible } => {
                if let Some(h) = indicator(&id) {
                    h.set_visible(visible);
                }
            }
            TreeCommand::OpenIndicatorSettings(id) => {
                chart.renderer().open_indicator_settings(&id);
            }
            TreeCommand::MoveIndicator { id, to } => {
                chart.panes().move_indicator(&id, move_target(&to));
            }
            TreeCommand::SetSeriesZ { id, z } => {
                chart
                    .renderer()
                    .set("seriesOrder", json!({ "id": id, "z": z }));
            }
            TreeCommand::RemoveIndicator(id) => {
                if let Some(h) = indicator(&id) {
                    h.remove();
                }
            }
            TreeCommand::ShowDrawing { id, visible } => {
                chart.drawings().show(&id, visible);
            }
            TreeCommand::LockDrawing { id, locked } => {
                chart.drawings().lock(&id, locked);
            }
            TreeCommand::DuplicateDrawing(id) => self.clone_drawing_into(&id),
            TreeCommand::BringDrawingToFront(id) => {
                chart.drawings().bring_to_front(&id);
            }
            TreeCommand::SendDrawingToBack(id) => {
                chart.drawings().send_to_back(&id);
            }
            TreeCommand::RemoveDrawing(id) => {
                chart.drawings().remove(&id);
            }
            TreeCommand::MakeGroup(ids) => {
                let tr = self.cx.tr();
                self.with_model(|m| m.make_group(&ids, tr.as_deref()));
            }
            TreeCommand::AssignToGroup { group_id, drawing } => {
                self.with_model(|m| m.assign(&group_id, &[drawing]));
            }
            TreeCommand::RemoveFromGroup(id) => {
                self.with_model(|m| m.unassign(&[id]));
            }
            TreeCommand::BeginRename(id) => {
                self.with_model(|m| m.begin_rename(&id));
            }
            TreeCommand::SetMembersVisible { ids, visible } => {
                self.set_members(
                    &ids,
                    DrawingUpdate {
                        visible: Some(visible),
                        ..Default::default()
                    },
                );
            }
            TreeCommand::SetMembersLocked { ids, locked } => {
                self.set_members(
                    &ids,
                    DrawingUpdate {
                        locked: Some(locked),
                        ..Default::default()
                    },
                );
            }
            TreeCommand::SelectAll(ids) => {
                self.with_model(|m| m.set_picked(&ids));
                self.select_on_chart(&ids);
            }
            TreeCommand::RestackGroup { group_id, to_front } => {
                let plan = {
                    let pass = self.pass.borrow();
                    let model = self.model_snapshot();
                    pass.as_ref().and_then(|p| {
                        let g = model.groups().iter().find(|g| g.id == group_id)?;
                        plan_restack_group(
                            g,
                            &p.panes,
                            &live_of(&p.drawings),
                            to_front,
                            p.flags.stackable,
                        )
                    })
                };
                if let Some(plan) = plan {
                    self.write_stack(&chart, plan);
                }
            }
            TreeCommand::RemoveGroup {
                group_id,
                with_members,
            } => {
                self.remove_group(&group_id, with_members);
                return;
            }
        }
        self.render();
    }

    /// The stacking writes of a plan: the series through the renderer's order settings, the
    /// drawings as ONE `update_many` (a single undo step).
    fn write_stack(&self, chart: &Vela, plan: StackPlan) {
        if let Some((id, to)) = &plan.move_indicator {
            chart.panes().move_indicator(id, move_target(to));
        }
        let stackable = self
            .pass
            .borrow()
            .as_ref()
            .is_some_and(|p| p.flags.stackable);
        if stackable {
            if let Some(z) = plan.writes.candle_z {
                chart.renderer().set("candleZOrder", json!(z));
            }
            for s in &plan.writes.series {
                chart
                    .renderer()
                    .set("seriesOrder", json!({ "id": s.id, "z": s.z }));
            }
        }
        if !plan.drawing_patches.is_empty() {
            chart.drawings().update_many(
                plan.drawing_patches
                    .iter()
                    .map(|p| {
                        (
                            p.id.clone(),
                            DrawingUpdate {
                                z_index: Some(p.z_index),
                                pane_id: p.pane_id.clone(),
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            );
        }
    }

    // ── pointer: drag, click, menus ──

    fn wire_body(self: &Rc<Self>) {
        let body = self.view.body.clone();
        let me = self.me.clone();
        self.static_bag
            .on(&body, "pointerdown", move |ev: JsValue| {
                if let Some(t) = me.upgrade() {
                    t.on_pointer_down(&ev);
                }
            });
        let me = self.me.clone();
        self.static_bag
            .on(&body, "contextmenu", move |ev: JsValue| {
                let Some(t) = me.upgrade() else { return };
                if t.press_opened.get() {
                    prevent_default(&ev);
                    return;
                }
                t.cancel_press();
                if t.open_row_menu(&ev, ev_f64(&ev, "clientX"), ev_f64(&ev, "clientY")) {
                    prevent_default(&ev);
                }
            });
        let me = self.me.clone();
        self.static_bag
            .on(&body, "pointermove", move |ev: JsValue| {
                let Some(t) = me.upgrade() else { return };
                let far = t.press.borrow().as_ref().is_some_and(|(x, y, _)| {
                    (ev_f64(&ev, "clientX") - x).hypot(ev_f64(&ev, "clientY") - y) > PRESS_SLOP
                });
                if far {
                    t.cancel_press();
                }
            });
        for name in ["pointerup", "pointercancel"] {
            let me = self.me.clone();
            self.static_bag.on(&body, name, move |_| {
                if let Some(t) = me.upgrade() {
                    t.cancel_press();
                }
            });
        }
    }

    fn cancel_press(&self) {
        if let Some((_, _, id)) = self.press.borrow_mut().take() {
            self.cx.root.platform().clear_timeout(id);
        }
    }

    /// Open the menu of the row under the event, at (x, y). False when there is none.
    fn open_row_menu(&self, ev: &JsValue, x: f64, y: f64) -> bool {
        let Some(target) = event_target(ev) else {
            return false;
        };
        let Some(row) = closest(&target, |e| has_class(e, "vela-ot-row")) else {
            return false;
        };
        let items = {
            let pass = self.pass.borrow();
            let Some(p) = pass.as_ref() else { return false };
            let model = self.model_snapshot();
            let drawing_z: HashMap<String, Vec<f64>> = if p.flags.interleave {
                let mut m: HashMap<String, Vec<f64>> = HashMap::new();
                for d in &p.drawings {
                    m.entry(d.pane_id.clone()).or_default().push(d.z_index);
                }
                m
            } else {
                HashMap::new()
            };
            let tr = self.cx.tr();
            let inp = MenuInputs {
                panes: &p.panes,
                groups: model.groups(),
                picked: model.picked(),
                stackable: p.flags.stackable,
                panes_supported: p.panes_supported,
                supports_indicator_settings: p.supports_indicator_settings,
                series_order: &p.series_order,
                candle_z: p.candle_z,
                drawing_z_by_pane: &drawing_z,
                i18n: tr.as_deref(),
            };
            let mut b = MenuBuilder::new();
            let pane_id = data(&row, "pane").unwrap_or_else(|| PRICE_PANE_ID.to_string());
            let id = data(&row, "id");
            let items = match data(&row, "row-kind").as_deref() {
                Some("price") => {
                    let visible = p
                        .chart
                        .renderer()
                        .get("candleVisible")
                        .and_then(|v| v.as_bool())
                        != Some(false);
                    price_menu(&mut b, &inp, visible)
                }
                Some("indicator") => id
                    .as_deref()
                    .and_then(|id| find_indicator_row(&p.panes, id))
                    .map(|(_, r)| indicator_menu(&mut b, &inp, &r.id, r.visible, &pane_id))
                    .unwrap_or_default(),
                Some("drawing") => id
                    .as_deref()
                    .and_then(|id| p.drawings.iter().find(|d| d.id == id))
                    .map(|d| drawing_menu(&mut b, &inp, d))
                    .unwrap_or_default(),
                Some("group") => id
                    .as_deref()
                    .and_then(|id| model.groups().iter().find(|g| g.id == id))
                    .map(|g| {
                        let members: Vec<SerializedDrawing> = p
                            .drawings
                            .iter()
                            .filter(|d| g.ids.contains(&d.id))
                            .cloned()
                            .collect();
                        group_menu(&mut b, &inp, g, &members)
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            *self.commands.borrow_mut() = std::mem::take(&mut b.commands);
            items
        };
        if items.is_empty() {
            return false;
        }
        self.close_menu();
        let me = self.me.clone();
        let label = self.tx(TreeText::ObjectTree);
        let handle = open_menu(
            &self.cx,
            &items,
            x,
            y,
            &label,
            None,
            Rc::new(move |id: &str| {
                if let Some(t) = me.upgrade() {
                    let cmd = t.commands.borrow().get(id).cloned();
                    if let Some(cmd) = cmd {
                        t.run_command(cmd);
                    }
                }
            }),
            None,
        );
        *self.menu.borrow_mut() = handle;
        true
    }

    fn on_pointer_down(&self, ev: &JsValue) {
        // A touch press that holds opens the row's menu.
        if crate::dom::get_str(ev, "pointerType") != "mouse"
            && !crate::dom::get_str(ev, "pointerType").is_empty()
        {
            self.press_opened.set(false);
            self.cancel_press();
            let (x, y) = (ev_f64(ev, "clientX"), ev_f64(ev, "clientY"));
            let ev2 = ev.clone();
            let me = self.me.clone();
            let id = self.cx.root.platform().set_timeout(
                LONG_PRESS_MS,
                Box::new(move || {
                    let Some(t) = me.upgrade() else { return };
                    *t.press.borrow_mut() = None;
                    if t.open_row_menu(&ev2, x, y) {
                        t.press_opened.set(true);
                    }
                }),
            );
            *self.press.borrow_mut() = Some((x, y, id));
        }
        if self.drag.borrow().is_some() || ev_f64(ev, "button") != 0.0 {
            return;
        }
        let Some(target) = event_target(ev) else {
            return;
        };
        // Row and pane-head buttons are controls, not drag handles.
        if closest(&target, |e| has_class(e, "vela-ot-btn")).is_some() {
            return;
        }
        let Some(row) = closest(&target, |e| {
            has_class(e, "vela-ot-row") && data(e, "drag").is_some()
        }) else {
            return;
        };
        if closest(&target, |e| has_class(e, "vela-ot-rename")).is_some() {
            return;
        }
        let kind = match data(&row, "row-kind").as_deref() {
            Some("price") => DragKind::Price,
            Some("indicator") => DragKind::Indicator,
            Some("drawing") => DragKind::Drawing,
            Some("group") => DragKind::Group,
            _ => return,
        };
        let Some(view) = default_view(&self.cx.doc) else {
            return;
        };
        let label = kids(&row)
            .into_iter()
            .find(|k| has_class(k, "vela-ot-name"))
            .and_then(|k| k.text_content())
            .unwrap_or_else(|| "object".to_string());
        let session = DragSession::new(
            kind,
            data(&row, "id"),
            &data(&row, "pane").unwrap_or_else(|| PRICE_PANE_ID.to_string()),
            &label,
            ev_f64(ev, "clientX"),
            ev_f64(ev, "clientY"),
        );
        let bag = Bag::new();
        for (name, apply) in [("pointermove", 0u8), ("pointerup", 1), ("pointercancel", 2)] {
            let me = self.me.clone();
            bag.push(crate::dom::Listener::new(
                &view,
                match name {
                    "pointermove" => "pointermove",
                    "pointerup" => "pointerup",
                    _ => "pointercancel",
                },
                false,
                false,
                move |e: JsValue| {
                    let Some(t) = me.upgrade() else { return };
                    match apply {
                        0 => t.on_drag_move(&e),
                        1 => t.end_drag(true),
                        _ => t.end_drag(false),
                    }
                },
            ));
        }
        *self.drag.borrow_mut() = Some(Drag {
            session,
            ghost: None,
            drop: None,
            geo: None,
            bag,
        });
        prevent_default(ev); // no text selection while dragging a row
    }

    fn on_drag_move(&self, ev: &JsValue) {
        let (x, y) = (ev_f64(ev, "clientX"), ev_f64(ev, "clientY"));
        let began = {
            let mut d = self.drag.borrow_mut();
            let Some(drag) = d.as_mut() else { return };
            drag.session.on_move(x, y)
        };
        let Some(began) = began else { return };
        if began {
            self.begin_drag();
        }
        let ghost = self.drag.borrow().as_ref().and_then(|d| d.ghost.clone());
        if let Some(g) = ghost {
            set_style(&g, "left", &format!("{}px", x + 12.0));
            set_style(&g, "top", &format!("{}px", y + 8.0));
        }
        let geo = self.measure();
        let resolved = {
            let pass = self.pass.borrow();
            let d = self.drag.borrow();
            match (pass.as_ref(), d.as_ref()) {
                (Some(p), Some(d)) => resolve_drop(
                    &d.session,
                    x,
                    y,
                    &geo.gaps.iter().map(|(_, g)| g.clone()).collect::<Vec<_>>(),
                    &geo.panes
                        .iter()
                        .map(|(_, g, _)| g.clone())
                        .collect::<Vec<_>>(),
                    &p.panes,
                    p.flags,
                ),
                _ => None,
            }
        };
        self.paint_drop_hint(resolved.as_ref(), Some(&geo));
        if let Some(d) = self.drag.borrow_mut().as_mut() {
            d.drop = resolved;
            d.geo = Some(geo);
        }
    }

    fn begin_drag(&self) {
        let body = self.view.body.clone();
        set_data(&body, "dragging", Some("1"));
        let label = self
            .drag
            .borrow()
            .as_ref()
            .map(|d| d.session.label.clone())
            .unwrap_or_default();
        if let Some(src) = self.source_row() {
            set_data(&src, "source", Some("1"));
        }
        let ghost = self.cx.text("div", "vela-ot-ghost", &label);
        let _ = self.cx.root.element().append_child(&ghost);
        if let Some(d) = self.drag.borrow_mut().as_mut() {
            d.ghost = Some(ghost);
        }
    }

    fn source_row(&self) -> Option<Element> {
        let d = self.drag.borrow();
        let d = d.as_ref()?;
        let mut rows = Vec::new();
        find_all(&self.view.body, &|e| has_class(e, "vela-ot-row"), &mut rows);
        rows.into_iter().find(|el| {
            let kind = match d.session.kind {
                DragKind::Price => "price",
                DragKind::Indicator => "indicator",
                DragKind::Drawing => "drawing",
                DragKind::Group => "group",
            };
            data(el, "row-kind").as_deref() == Some(kind)
                && (d.session.id.is_none() || data(el, "id") == d.session.id)
        })
    }

    fn rect_of(el: &Element) -> Rect {
        let r = el.get_bounding_client_rect();
        Rect::new(
            r.left(),
            r.top(),
            r.left() + r.width(),
            r.top() + r.height(),
        )
    }

    /// The rendered geometry the drop resolution reads.
    fn measure(&self) -> Geo {
        let mut gaps = Vec::new();
        let mut gap_els = Vec::new();
        find_all(
            &self.view.body,
            &|e| has_class(e, "vela-ot-gap"),
            &mut gap_els,
        );
        for g in gap_els {
            let geo = GapGeometry {
                before: data(&g, "before"),
                after: data(&g, "after"),
                rect: Self::rect_of(&g),
            };
            gaps.push((g, geo));
        }
        let pass = self.pass.borrow();
        let mut panes = Vec::new();
        let mut blocks = Vec::new();
        find_all(
            &self.view.body,
            &|e| has_class(e, "vela-ot-pane"),
            &mut blocks,
        );
        for block in blocks {
            let pane_id = data(&block, "pane").unwrap_or_else(|| PRICE_PANE_ID.to_string());
            let kind = if data(&block, "kind").as_deref() == Some("price") {
                PaneKind::Price
            } else {
                PaneKind::Study
            };
            let stack = kids(&block)
                .into_iter()
                .find(|k| has_class(k, "vela-ot-stack"));
            let mut units = Vec::new();
            let mut unit_els = Vec::new();
            for u in stack.map(|s| kids(&s)).unwrap_or_default() {
                let subs: Vec<Element> = kids(&u)
                    .into_iter()
                    .filter(|k| has_class(k, "vela-ot-subrow"))
                    .collect();
                units.push(UnitGeometry {
                    rect: Self::rect_of(&u),
                    group_id: data(&u, "group"),
                    subrows: subs.iter().map(Self::rect_of).collect(),
                });
                unit_els.push((u, subs));
            }
            let _ = &pass;
            panes.push((
                block.clone(),
                PaneGeometry {
                    pane_id,
                    kind,
                    rect: Self::rect_of(&block),
                    units,
                },
                unit_els,
            ));
        }
        Geo { gaps, panes }
    }

    fn paint_drop_hint(&self, drop: Option<&DropAt>, geo: Option<&Geo>) {
        let mut marked = Vec::new();
        find_all(
            &self.view.body,
            &|e| e.has_attribute("data-drop"),
            &mut marked,
        );
        for el in marked {
            set_data(&el, "drop", None);
        }
        let (Some(drop), Some(geo)) = (drop, geo) else {
            return;
        };
        match drop {
            DropAt::NewPane { before, after } => {
                if let Some((el, _)) = geo
                    .gaps
                    .iter()
                    .find(|(_, g)| g.before == *before && g.after == *after)
                {
                    set_data(el, "drop", Some("gap"));
                }
            }
            DropAt::Merge { pane_id } => {
                if let Some((el, _, _)) = geo.panes.iter().find(|(_, g, _)| g.pane_id == *pane_id) {
                    set_data(el, "drop", Some("target"));
                }
            }
            DropAt::IntoGroup {
                pane_id,
                group_id,
                member_slot,
            } => {
                if let Some((_, _, units)) =
                    geo.panes.iter().find(|(_, g, _)| g.pane_id == *pane_id)
                    && let Some((unit, subs)) = units
                        .iter()
                        .find(|(u, _)| data(u, "group").as_deref() == Some(group_id.as_str()))
                {
                    set_data(unit, "drop", Some("target"));
                    if let Some(at) = subs.get(*member_slot) {
                        set_data(at, "drop", Some("before"));
                    } else if let Some(last) = subs.last() {
                        set_data(last, "drop", Some("after"));
                    }
                }
            }
            DropAt::Slot { pane_id, slot } => {
                if let Some((block, _, units)) =
                    geo.panes.iter().find(|(_, g, _)| g.pane_id == *pane_id)
                {
                    if units.is_empty() {
                        set_data(block, "drop", Some("target"));
                    } else if let Some((at, _)) = units.get(*slot) {
                        set_data(at, "drop", Some("before"));
                    } else if let Some((last, _)) = units.last() {
                        set_data(last, "drop", Some("after"));
                    }
                }
            }
        }
    }

    /// Close the session out: `apply` false abandons the move.
    fn end_drag(&self, apply: bool) {
        let drag = self.drag.borrow_mut().take();
        let Some(drag) = drag else { return };
        drag.bag.retire(&self.cx);
        // A press that never crossed the threshold is a plain click: leave the DOM as it is.
        if !drag.session.active() {
            return;
        }
        if let Some(g) = &drag.ghost {
            g.remove();
        }
        set_data(&self.view.body, "dragging", None);
        let src = {
            // The session is gone from the cell: find the source by the saved session.
            let mut rows = Vec::new();
            find_all(&self.view.body, &|e| has_class(e, "vela-ot-row"), &mut rows);
            rows.into_iter().find(|e| data(e, "source").is_some())
        };
        if let Some(s) = src {
            set_data(&s, "source", None);
        }
        self.paint_drop_hint(None, None);
        if apply && let Some(drop) = &drag.drop {
            self.apply_drop(&drag.session, drop);
        }
        self.refresh();
    }

    fn apply_drop(&self, session: &DragSession, drop: &DropAt) {
        let plan = {
            let pass = self.pass.borrow();
            let Some(p) = pass.as_ref() else { return };
            let id = self.cell.borrow().clone();
            let mut models = self.models.borrow_mut();
            let model = models.entry(id).or_default();
            plan_drop(
                model,
                session,
                drop,
                &p.panes,
                p.chart.drawings().supported(),
                &live_of(&p.drawings),
                p.flags.stackable,
            )
        };
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        match plan {
            DropPlan::None => {}
            DropPlan::MoveIndicator { id, to } => {
                chart.panes().move_indicator(&id, move_target(&to));
            }
            DropPlan::Stack(plan) => self.write_stack(&chart, plan),
        }
    }
}

fn live_of(drawings: &[SerializedDrawing]) -> Vec<LiveDrawing> {
    drawings
        .iter()
        .map(|d| LiveDrawing {
            id: d.id.clone(),
            pane_id: d.pane_id.clone(),
        })
        .collect()
}

fn move_target(to: &IndicatorTarget) -> MoveTarget {
    match to {
        IndicatorTarget::Price => MoveTarget::Price,
        IndicatorTarget::Pane(id) => MoveTarget::Pane(id.clone()),
        IndicatorTarget::NewPane { before, after } => MoveTarget::NewPane(NewPanePlacement {
            before: before.clone(),
            after: after.clone(),
        }),
    }
}

/// A round ticker icon: the provider-resolved image when `url` is given and has not failed
/// before, a colored initials badge otherwise.
pub fn ticker_icon_el(cx: &Cx, base: &str, name: &str, class: &str, url: Option<&str>) -> Element {
    use vela_core::widget::symbol_icon::{IconFailures, TickerIcon, ticker_icon};
    thread_local! {
        static FAILURES: IconFailures = IconFailures::new();
    }
    let wrap = cx.el("span", class);
    let icon = FAILURES.with(|f| ticker_icon(f, base, name, url));
    match icon {
        TickerIcon::Initials { text, background } => {
            set_style(&wrap, "background", background);
            wrap.set_text_content(Some(&text));
        }
        TickerIcon::Image { url } => {
            let img = cx.el("img", "");
            attr(&img, "alt", "");
            attr(&img, "src", &url);
            attr(&img, "crossorigin", "anonymous");
            crate::dom::set_styles(
                &img,
                &[
                    ("width", "100%"),
                    ("height", "100%"),
                    ("border-radius", "50%"),
                    ("display", "block"),
                    ("object-fit", "cover"),
                ],
            );
            let (w, u, base, name) = (
                wrap.clone(),
                url.clone(),
                base.to_string(),
                name.to_string(),
            );
            let cx2 = cx.clone();
            let l = crate::dom::on(&img, "error", move |_| {
                FAILURES.with(|f| f.mark_failed(&u));
                if let TickerIcon::Initials { text, background } =
                    vela_core::widget::symbol_icon::initials_badge(&base, &name)
                {
                    clear(&w);
                    set_style(&w, "background", background);
                    w.set_text_content(Some(&text));
                }
                let _ = &cx2;
            });
            // The image lives as long as the wrapper: park the listener on it.
            std::mem::forget(l);
            let _ = wrap.append_child(&img);
        }
    }
    wrap
}

// Keep `JsCast` imported for `dyn_ref` above.
#[allow(dead_code)]
fn _cast(v: &JsValue) -> Option<&js_sys::Function> {
    v.dyn_ref::<js_sys::Function>()
}

/// The symbol's base: the quote / contract suffix of the ticker stripped
/// (a trailing USDT, USDC, USD1, USDS, BUSD, USD, EUR or PERP, with an optional separator before
/// it), the ticker itself when nothing is left.
pub fn strip_quote(symbol: &str) -> String {
    const SUFFIXES: [&str; 7] = ["USDT", "USDC", "USD1", "USDS", "BUSD", "USD", "EUR"];
    let upper = symbol.to_uppercase();
    let mut end = symbol.len();
    if upper.ends_with("PERP") {
        end -= 4;
    } else if let Some(s) = SUFFIXES.iter().find(|s| upper.ends_with(**s)) {
        end -= s.len();
    } else {
        return symbol.to_string();
    }
    let head = &symbol[..end];
    let head = head.strip_suffix(['-', '_', '/']).unwrap_or(head);
    if head.is_empty() {
        symbol.to_string()
    } else {
        head.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::strip_quote;

    #[test]
    fn the_quote_suffix_goes_the_ticker_stays_when_nothing_is_left() {
        assert_eq!(strip_quote("BTCUSDT"), "BTC");
        assert_eq!(strip_quote("eth-usd"), "eth");
        assert_eq!(strip_quote("SOL/USDC"), "SOL");
        assert_eq!(strip_quote("BTC-PERP"), "BTC");
        assert_eq!(strip_quote("USDT"), "USDT");
        assert_eq!(strip_quote("AAA"), "AAA");
    }
}
