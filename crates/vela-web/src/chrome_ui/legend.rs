//! The indicator legend: a port of `InputsUI.ts` without the `vela-ui` kit.
//!
//! One column per pane (`.vela-legend[data-vela-pane]`) pinned under the pane's top edge,
//! one chip per indicator (`.vela-ind-row`): title, host callouts, status (load dots or the
//! live pulse), the plot values (colored, live with the crosshair) and, while the row is
//! open (title hover, selection or keyboard focus), the action cluster: eye, settings gear,
//! "move to" menu, host actions, remove. The price pane's column ends with the fold chip.
//!
//! What the original reads from hooks is pulled here: the gear exists only for an indicator
//! whose model has inputs or props (`indicator_model`), the "move to" menu lists
//! `pane_views`, the restack entries come from `layer_above` / `layer_below`, the host
//! actions and callouts from the providers. Those pulls run from the deferred flush or from
//! DOM handlers, never inside a host call (the renderer is borrowed there).
//!
//! Differences from the original:
//!
//! - Rows are focusable (`tabindex=0`, `role=group`): focus opens the row, so the action
//!   cluster is reachable from the keyboard (Enter selects, Escape deselects). The original
//!   reveals it by pointer only.
//! - The eye and the "move to" button are always offered (the original gates them on hooks
//!   the host wires); the core always supports both.
//! - No "beta" mark (the legend feed does not carry the flag).
//! - Load dots animate through CSS, not the Web Animations API.
//! - The settings dialog is the dialog author's: the gear calls
//!   `ChartRenderer::open_indicator_settings`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use vela_core::i18n::{MessageArg, msg};
use vela_core::indicators::IndicatorStatus;
use vela_core::model::PaneKind;
use vela_core::ports::chart_renderer::{
    ChartRenderer, LegendCalloutPanelItem, LegendCalloutView, MoveTarget, NewPanePlacement,
};
use vela_core::render::core::overlay_stack::Neighbor;
use vela_core::render::native::{LegendPlotValue, LegendRow, NativeRenderer, PaneView};
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, HtmlElement};

use super::ChromeUi;
use super::D_LAYOUT;
use super::icons::icon_at;
use super::menu::MenuItem;
use super::popover::{Placement as PopPlacement, Popover};
use super::tooltip::{self, Placement, Tip};
use crate::dom::{
    Listener, attr, get_f64, get_prop, get_str, make, on, prevent_default, set_style,
    stop_propagation, vela_contains,
};

const ICON_PX: u32 = 16;
const STACK_RAISE: &str = "stack:raise";
const STACK_LOWER: &str = "stack:lower";

/// An open callout panel (at most one).
pub struct OpenCallout {
    pop: Popover,
    row_id: String,
    anchor: Element,
    _held: Rc<RefCell<Vec<Listener>>>,
}

struct Callout {
    el: Element,
    _listeners: Vec<Listener>,
    _tip: Tip,
}

struct Extra {
    _listener: Listener,
    _tip: Tip,
}

struct Row {
    id: String,
    seq: u64,
    title: String,
    pane_id: String,
    /// A core-computed native indicator: its row prepends (pinned to the top of its
    /// column), a script's appends.
    native: bool,
    hidden: bool,
    highlighted: bool,
    has_settings: bool,
    show_values: Option<bool>,
    plot_values: Vec<LegendPlotValue>,
    plot_key: String,
    el: Element,
    title_el: Element,
    status_el: Element,
    callouts_el: Element,
    values_el: Element,
    extras_el: Element,
    eye: Element,
    gear: Element,
    move_btn: Element,
    close_btn: Element,
    _listeners: Vec<Listener>,
    tips: Vec<Tip>,
    extras: Vec<Extra>,
    callouts: Vec<Callout>,
}

struct Fold {
    el: Element,
    _listener: Listener,
    _tip: Tip,
}

pub struct Legend {
    containers: HashMap<String, Element>,
    rows: HashMap<String, Row>,
    seq: u64,
    selected: Option<String>,
    folded: bool,
    titles_visible: bool,
    values_visible: bool,
    mobile: bool,
    collapse: HashMap<String, Option<String>>,
    order: Vec<String>,
    order_key: String,
    fold: Option<Fold>,
    overview: bool,
    doc_click: Option<Listener>,
    pending: HashSet<String>,
    all_pending: bool,
}

impl Legend {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
            rows: HashMap::new(),
            seq: 0,
            selected: None,
            folded: false,
            titles_visible: true,
            values_visible: true,
            mobile: false,
            collapse: HashMap::new(),
            order: Vec::new(),
            order_key: String::new(),
            fold: None,
            overview: false,
            doc_click: None,
            pending: HashSet::new(),
            all_pending: false,
        }
    }

    fn row_shows_values(&self, row: &Row) -> bool {
        row.show_values.unwrap_or(self.values_visible)
    }
}

/// Place a row in its pane's column: native rows prepend, a script's append.
fn attach(lg: &Element, el: &Element, native: bool) {
    if native {
        let _ = lg.prepend_with_node_1(el);
    } else {
        let _ = lg.append_child(el);
    }
}

/// Whether a click landed on a `<button>` inside `row`.
fn target_in_button(ev: &JsValue, row: &Element) -> bool {
    let mut node = crate::dom::target_element(&get_prop(ev, "target"));
    while let Some(n) = node {
        if &n == row {
            return false;
        }
        if n.tag_name().eq_ignore_ascii_case("button") {
            return true;
        }
        node = n.parent_element();
    }
    false
}

fn render_values(doc: &Document, row: &mut Row, shown: bool) {
    if !shown || row.plot_values.is_empty() || row.highlighted {
        set_style(&row.values_el, "display", "none");
        row.values_el.set_text_content(Some(""));
        return;
    }
    // Inline values in a block (not flex items), so a readout cut at the plot edge ends in
    // an ellipsis rather than a clipped number.
    set_style(&row.values_el, "display", "block");
    row.values_el.set_text_content(Some(""));
    // Each value is a left-to-right isolate, so a sign stays in front of its number in a
    // right-to-left page.
    for v in &row.plot_values {
        let outer = make(doc, "span", "");
        set_style(&outer, "color", &v.color);
        let inner = make(doc, "span", "");
        attr(&inner, "dir", "ltr");
        inner.set_text_content(Some(&v.value));
        let _ = outer.append_child(&inner);
        let _ = row.values_el.append_child(&outer);
    }
}

/// Reveal the action cluster; the status steps aside while it is out. Returns whether the
/// row's callout panel must close.
fn sync_actions(row: &Row) -> bool {
    let open = row.highlighted;
    attr(&row.el, "data-open", if open { "1" } else { "0" });
    attr(&row.el, "data-hidden", if row.hidden { "1" } else { "0" });
    if open {
        let _ = row.el.append_child(&row.status_el);
    } else {
        let _ = row.el.insert_before(&row.status_el, Some(&row.values_el));
    }
    set_style(
        &row.callouts_el,
        "display",
        if !open && !row.callouts.is_empty() {
            "inline-flex"
        } else {
            "none"
        },
    );
    open
}

impl ChromeUi {
    // ── texts ──

    fn lt(&self, key: &str) -> String {
        self.text(&match key {
            "hide" => msg("chart.legend.hide", "Hide"),
            "show" => msg("chart.legend.show", "Show"),
            "settings" => msg("chart.legend.settings", "Settings"),
            "move" => msg("chart.legend.moveToPaneButton", "Move to panel"),
            "remove" => msg("chart.legend.remove", "Remove indicator"),
            "values" => msg("chart.legend.indicatorValues", "Indicator values"),
            "toMain" => msg("chart.legend.moveToMainChart", "Move to main chart"),
            "newAbove" => msg("chart.legend.newPaneAbove", "New panel above"),
            "newBelow" => msg("chart.legend.newPaneBelow", "New panel below"),
            "frontPrice" => msg(
                "chart.legend.bringInFrontOfPrice",
                "Bring in front of price",
            ),
            "frontNext" => msg(
                "chart.legend.bringInFrontOfNext",
                "Bring in front of the indicator in front",
            ),
            "backPrice" => msg("chart.legend.sendBehindPrice", "Send behind price"),
            "backNext" => msg(
                "chart.legend.sendBehindNext",
                "Send behind the indicator behind",
            ),
            "indicators" => msg("chart.legend.indicators", "Indicators"),
            "showLegend" => msg("chart.legend.showLegend", "Show indicator legend"),
            _ => msg("chart.legend.hideLegend", "Hide indicator legend"),
        })
    }

    fn lt_with(&self, key: &str, name: &str) -> String {
        let (m, arg) = match key {
            "toPane" => (msg("chart.legend.moveToPane", "Move to {pane}"), "pane"),
            "front" => (
                msg(
                    "chart.legend.bringInFrontOf",
                    "Bring in front of {indicator}",
                ),
                "indicator",
            ),
            _ => (
                msg("chart.legend.sendBehind", "Send behind {indicator}"),
                "indicator",
            ),
        };
        self.text_with(&m, &[(arg, MessageArg::from(name.to_string()))])
    }

    fn fold_label(&self, st: &Legend) -> String {
        self.lt(if st.overview {
            "indicators"
        } else if st.folded {
            "showLegend"
        } else {
            "hideLegend"
        })
    }

    // ── containers ──

    fn legend_for(&self, st: &mut Legend, pane_id: &str) -> Element {
        if let Some(lg) = st.containers.get(pane_id) {
            return lg.clone();
        }
        let lg = make(&self.doc, "div", "vela-legend");
        // Tagged with the pane id so a host app can locate each pane's legend.
        attr(&lg, "data-vela-pane", pane_id);
        attr(&lg, "role", "group");
        attr(&lg, "aria-label", &self.lt("indicators"));
        let _ = self.root.append_child(&lg);
        st.containers.insert(pane_id.to_string(), lg.clone());
        lg
    }

    fn drop_empty_container(st: &mut Legend, pane_id: &str) {
        if pane_id == "price" {
            return;
        }
        if !st.rows.values().any(|r| r.pane_id == pane_id)
            && let Some(lg) = st.containers.remove(pane_id)
        {
            lg.remove();
        }
    }

    // ── host feed ──

    pub(crate) fn legend_upsert(self: &Rc<Self>, spec: &LegendRow) {
        let pane_id = spec.pane_id.clone().unwrap_or_else(|| "price".to_string());
        let mut moved = false;
        let existing = self.legend.borrow().rows.contains_key(&spec.id);
        if existing {
            let mut st = self.legend.borrow_mut();
            if let Some(row) = st.rows.get_mut(&spec.id) {
                row.title = spec.title.clone();
                row.title_el.set_text_content(Some(&spec.title));
                attr(&row.el, "aria-label", &spec.title);
                if row.pane_id != pane_id {
                    row.pane_id = pane_id.clone();
                    moved = true;
                }
            }
        } else {
            // Built before the borrow: building registers the document listener.
            let seq = {
                let mut st = self.legend.borrow_mut();
                st.seq += 1;
                st.seq - 1
            };
            let row = self.build_row(spec, &pane_id, seq);
            let mut st = self.legend.borrow_mut();
            let lg = self.legend_for(&mut st, &pane_id);
            attach(&lg, &row.el, row.native);
            st.rows.insert(spec.id.clone(), row);
            st.pending.insert(spec.id.clone());
            st.order_key.clear();
        }
        if moved {
            self.attach_row_to_pane(&spec.id, &pane_id);
        }
        self.sync_fold_toggle();
        self.legend_apply_order();
        self.schedule(super::D_ROWS | D_LAYOUT);
    }

    fn attach_row_to_pane(&self, id: &str, pane_id: &str) {
        let mut st = self.legend.borrow_mut();
        let lg = self.legend_for(&mut st, pane_id);
        let prev = st.rows.get(id).map(|r| (r.el.clone(), r.native));
        if let Some((el, native)) = prev {
            attach(&lg, &el, native);
        }
        let panes: Vec<String> = st.containers.keys().cloned().collect();
        for p in panes {
            Self::drop_empty_container(&mut st, &p);
        }
        st.order_key.clear();
    }

    pub(crate) fn legend_remove(self: &Rc<Self>, id: &str) {
        let close_callout = self
            .callout_panel
            .borrow()
            .as_ref()
            .is_some_and(|c| c.row_id == id);
        let removed = {
            let mut st = self.legend.borrow_mut();
            let Some(row) = st.rows.remove(id) else {
                return;
            };
            row.el.remove();
            if st.selected.as_deref() == Some(id) {
                st.selected = None;
            }
            st.pending.remove(id);
            st.order_key.clear();
            let pane = row.pane_id.clone();
            Self::drop_empty_container(&mut st, &pane);
            row
        };
        if close_callout {
            self.close_callout_panel();
        }
        for tip in removed
            .tips
            .iter()
            .chain(removed.callouts.iter().map(|c| &c._tip))
        {
            tip.dismiss();
        }
        // The close button may be what called us: free the closures later.
        self.retire(Box::new(removed));
        self.sync_fold_toggle();
        self.schedule(D_LAYOUT);
    }

    pub(crate) fn legend_set_status(&self, id: &str, status: IndicatorStatus) {
        let mut st = self.legend.borrow_mut();
        let Some(row) = st.rows.get_mut(id) else {
            return;
        };
        row.status_el.set_text_content(Some(""));
        let name = match status {
            IndicatorStatus::Idle => "idle",
            IndicatorStatus::Loading => "loading",
            IndicatorStatus::Live => "live",
        };
        attr(&row.status_el, "data-status", name);
        if status == IndicatorStatus::Loading {
            for _ in 0..3 {
                let dot = make(&self.doc, "span", "");
                let _ = row.status_el.append_child(&dot);
            }
        }
    }

    pub(crate) fn legend_set_pane(self: &Rc<Self>, id: &str, pane_id: &str) {
        {
            let mut st = self.legend.borrow_mut();
            let Some(row) = st.rows.get_mut(id) else {
                return;
            };
            if row.pane_id == pane_id {
                return;
            }
            row.pane_id = pane_id.to_string();
        }
        self.attach_row_to_pane(id, pane_id);
        self.sync_fold_toggle();
        self.legend_apply_order();
        self.schedule(D_LAYOUT);
    }

    pub(crate) fn legend_set_visible(&self, id: &str, visible: bool) {
        let label = self.lt(if visible { "hide" } else { "show" });
        let mut st = self.legend.borrow_mut();
        let Some(row) = st.rows.get_mut(id) else {
            return;
        };
        row.hidden = !visible;
        attr(&row.eye, "aria-label", &label);
        row.eye
            .set_inner_html(&icon_at(if visible { "eye" } else { "eye-off" }, ICON_PX));
        sync_actions(row);
    }

    pub(crate) fn legend_relabel(&self, titles: &[(String, String)]) {
        let mut st = self.legend.borrow_mut();
        for (id, title) in titles {
            if let Some(row) = st.rows.get_mut(id)
                && row.title != *title
            {
                row.title = title.clone();
                row.title_el.set_text_content(Some(title));
                attr(&row.el, "aria-label", title);
            }
        }
    }

    pub(crate) fn legend_order(&self, ids: &[String]) {
        self.legend.borrow_mut().order = ids.to_vec();
        self.legend_apply_order();
    }

    /// List each pane's rows front-most first (the object tree's order); rows the order
    /// does not name keep their place after the others.
    fn legend_apply_order(&self) {
        let mut st = self.legend.borrow_mut();
        let mut members: Vec<(&String, &Row)> = st.rows.iter().collect();
        members.sort_by_key(|(_, r)| r.seq);
        let key = format!(
            "{}|{}",
            st.order.join(","),
            members
                .iter()
                .map(|(id, r)| format!("{id}@{}", r.pane_id))
                .collect::<Vec<_>>()
                .join(",")
        );
        if key == st.order_key {
            return;
        }
        let rank: HashMap<&str, usize> = st
            .order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let fold_el = st.fold.as_ref().map(|f| f.el.clone());
        for (pane, lg) in &st.containers {
            let mut rows: Vec<&Row> = st
                .rows
                .values()
                .filter(|r| &r.pane_id == pane && r.el.parent_element().as_ref() == Some(lg))
                .collect();
            // `order` is the paint order, back to front: the front-most row lists first,
            // rows it does not name follow in their current DOM order.
            let at_dom = |r: &Row| {
                let mut i = 0u32;
                let mut node = lg.first_element_child();
                while let Some(n) = node {
                    if n == r.el {
                        return i;
                    }
                    i += 1;
                    node = n.next_element_sibling();
                }
                u32::MAX
            };
            rows.sort_by(
                |a, b| match (rank.get(a.id.as_str()), rank.get(b.id.as_str())) {
                    (Some(ra), Some(rb)) => rb.cmp(ra),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => at_dom(a).cmp(&at_dom(b)),
                },
            );
            let anchor = fold_el
                .as_ref()
                .filter(|f| f.parent_element().as_ref() == Some(lg));
            for r in rows {
                let _ = lg.insert_before(&r.el, anchor.map(|a| a.as_ref()));
            }
        }
        st.order_key = key;
    }

    pub(crate) fn legend_collapsed(&self, masters: &[(String, Option<String>)]) {
        self.legend.borrow_mut().collapse = masters.iter().cloned().collect();
        self.schedule(D_LAYOUT);
    }

    pub(crate) fn legend_plot_values(&self, values: &[(String, Vec<LegendPlotValue>)]) {
        let doc = self.doc.clone();
        let mut st = self.legend.borrow_mut();
        let by_id: HashMap<&str, &Vec<LegendPlotValue>> =
            values.iter().map(|(id, v)| (id.as_str(), v)).collect();
        let default_shown = st.values_visible;
        for row in st.rows.values_mut() {
            let next: Vec<LegendPlotValue> = by_id
                .get(row.id.as_str())
                .map(|v| (*v).clone())
                .unwrap_or_default();
            let key = next
                .iter()
                .map(|v| format!("{}|{}", v.value, v.color))
                .collect::<Vec<_>>()
                .join("\u{0}");
            if key == row.plot_key {
                continue;
            }
            row.plot_values = next;
            row.plot_key = key;
            let shown = row.show_values.unwrap_or(default_shown);
            render_values(&doc, row, shown);
        }
    }

    pub(crate) fn legend_providers_changed(&self) {
        self.legend.borrow_mut().all_pending = true;
        self.schedule(super::D_ROWS);
    }

    pub(crate) fn legend_set_titles_visible(&self, visible: bool) {
        {
            let mut st = self.legend.borrow_mut();
            if st.titles_visible == visible {
                return;
            }
            st.titles_visible = visible;
        }
        self.schedule(D_LAYOUT);
    }

    /// "All" is literal: per-row overrides are cleared.
    pub(crate) fn legend_set_values_visible(&self, visible: bool) {
        let doc = self.doc.clone();
        let mut st = self.legend.borrow_mut();
        st.values_visible = visible;
        for row in st.rows.values_mut() {
            row.show_values = None;
            render_values(&doc, row, visible);
        }
    }

    pub(crate) fn legend_set_mobile(self: &Rc<Self>, mobile: bool) {
        {
            let mut st = self.legend.borrow_mut();
            if st.mobile == mobile {
                return;
            }
            st.mobile = mobile;
            // The fold resets to the mode's presentation default on a flip.
            if !st.overview {
                st.folded = mobile;
            }
        }
        self.sync_fold_toggle();
        self.schedule(D_LAYOUT);
    }

    pub(crate) fn legend_relabel_texts(&self) {
        let hide = self.lt("hide");
        let show = self.lt("show");
        let settings = self.lt("settings");
        let mv = self.lt("move");
        let remove = self.lt("remove");
        let mut st = self.legend.borrow_mut();
        for row in st.rows.values_mut() {
            attr(
                &row.eye,
                "aria-label",
                if row.hidden { &show } else { &hide },
            );
            attr(&row.gear, "aria-label", &settings);
            attr(&row.move_btn, "aria-label", &mv);
            attr(&row.close_btn, "aria-label", &remove);
        }
        let label = self.fold_label(&st);
        if let Some(fold) = &st.fold {
            attr(&fold.el, "aria-label", &label);
        }
    }

    pub(crate) fn legend_dispose(&self) {
        let mut st = self.legend.borrow_mut();
        for lg in st.containers.values() {
            lg.remove();
        }
        st.containers.clear();
        st.rows.clear();
        st.fold = None;
        st.doc_click = None;
        st.selected = None;
    }

    // ── deferred pulls ──

    /// Resolve the gear, the host actions and the callouts of the rows that asked.
    pub(crate) fn legend_flush_rows(self: &Rc<Self>, renderer: Option<&NativeRenderer>) {
        let Some(renderer) = renderer else { return };
        let (ids, all) = {
            let mut st = self.legend.borrow_mut();
            let all = std::mem::take(&mut st.all_pending);
            let ids: Vec<String> = if all {
                st.rows.keys().cloned().collect()
            } else {
                st.pending.drain().collect()
            };
            st.pending.clear();
            (ids, all)
        };
        // The overview action replaces the fold toggle's behavior.
        let overview = renderer.legend_overview_action().is_some();
        let flipped = {
            let mut st = self.legend.borrow_mut();
            let flipped = st.overview != overview;
            if flipped {
                st.overview = overview;
                st.folded = overview || st.mobile;
            }
            flipped
        };
        if flipped || all {
            self.sync_fold_toggle();
        }
        let actions = renderer.legend_actions_provider();
        let callouts = renderer.legend_callouts_provider();
        for id in ids {
            let model = renderer.indicator_model(&id);
            let has_settings = model
                .as_ref()
                .is_some_and(|m| !m.inputs.is_empty() || !m.props.is_empty());
            let action_views = actions.as_ref().map(|p| p(&id)).unwrap_or_default();
            let callout_views = callouts.as_ref().map(|p| p(&id)).unwrap_or_default();
            self.apply_row_pulls(&id, has_settings, action_views, callout_views);
        }
    }

    fn apply_row_pulls(
        self: &Rc<Self>,
        id: &str,
        has_settings: bool,
        actions: Vec<vela_core::ports::chart_renderer::LegendActionView>,
        callouts: Vec<LegendCalloutView>,
    ) {
        let mut stale: Vec<Box<dyn std::any::Any>> = Vec::new();
        {
            let mut st = self.legend.borrow_mut();
            let Some(row) = st.rows.get_mut(id) else {
                return;
            };
            row.has_settings = has_settings;
            if has_settings {
                crate::dom::remove_style(&row.gear, "display");
            } else {
                set_style(&row.gear, "display", "none");
            }
            // Host actions.
            for e in std::mem::take(&mut row.extras) {
                e._tip.dismiss();
                stale.push(Box::new(e));
            }
            row.extras_el.set_text_content(Some(""));
            for action in actions {
                let btn = make(&self.doc, "button", "vela-ind-ctl");
                attr(&btn, "type", "button");
                attr(&btn, "aria-label", &action.tooltip);
                attr(&btn, "data-legend-action", &action.id);
                btn.set_inner_html(&icon_at(&action.icon, ICON_PX));
                let run = action.run.clone();
                let listener = on(&btn, "click", move |ev: JsValue| {
                    stop_propagation(&ev); // selecting the row is not the intent
                    run();
                });
                let text = action.tooltip.clone();
                let tip =
                    tooltip::attach(self, &btn, move || text.clone(), Placement::Below, false);
                let _ = row.extras_el.append_child(&btn);
                row.extras.push(Extra {
                    _listener: listener,
                    _tip: tip,
                });
            }
            // Callouts.
            for c in std::mem::take(&mut row.callouts) {
                c._tip.dismiss();
                c.el.remove();
                stale.push(Box::new(c));
            }
            row.callouts_el.set_text_content(Some(""));
            for view in callouts {
                let c = self.build_callout(id, &view);
                let _ = row.callouts_el.append_child(&c.el);
                row.callouts.push(c);
            }
            sync_actions(row);
        }
        if !stale.is_empty() {
            self.retire(Box::new(stale));
        }
    }

    fn build_callout(self: &Rc<Self>, row_id: &str, view: &LegendCalloutView) -> Callout {
        let el = make(&self.doc, "span", "vela-callout");
        attr(&el, "data-slot", "callout-bubble");
        attr(&el, "aria-label", &view.tooltip);
        attr(&el, "data-legend-callout", &view.id);
        set_style(&el, "background", &view.background);
        if let Some(color) = &view.color {
            set_style(&el, "color", color);
        }
        el.set_inner_html(&icon_at(&view.icon, 12));
        let mut listeners = Vec::new();
        if let Some(panel) = view.content.clone() {
            attr(&el, "role", "button");
            attr(&el, "tabindex", "0");
            let toggle = {
                let weak = self.weak();
                let el = el.clone();
                let row_id = row_id.to_string();
                let label = view.tooltip.clone();
                Rc::new(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.toggle_callout_panel(&row_id, &el, &label, &panel);
                    }
                })
            };
            let t = toggle.clone();
            listeners.push(on(&el, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                t();
            }));
            listeners.push(on(&el, "dblclick", |ev: JsValue| stop_propagation(&ev)));
            listeners.push(on(&el, "keydown", move |ev: JsValue| {
                let key = get_str(&ev, "key");
                if key == "Enter" || key == " " {
                    prevent_default(&ev);
                    toggle();
                }
            }));
        }
        let text = view.tooltip.clone();
        let tip = tooltip::attach(self, &el, move || text.clone(), Placement::Below, false);
        Callout {
            el,
            _listeners: listeners,
            _tip: tip,
        }
    }

    pub(crate) fn close_callout_panel(&self) {
        let open = self.callout_panel.borrow_mut().take();
        if let Some(open) = open {
            open.pop.remove();
            self.retire(Box::new(open));
        }
    }

    fn toggle_callout_panel(
        self: &Rc<Self>,
        row_id: &str,
        anchor: &Element,
        label: &str,
        panel: &vela_core::ports::chart_renderer::LegendCalloutPanel,
    ) {
        let same_anchor = self
            .callout_panel
            .borrow()
            .as_ref()
            .is_some_and(|c| c.anchor == *anchor);
        self.close_callout_panel();
        if same_anchor {
            return;
        }
        let weak = self.weak();
        let dismiss: Rc<dyn Fn()> = Rc::new({
            let weak = weak.clone();
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.close_callout_panel();
                }
            }
        });
        let doc = self.doc.clone();
        let held: Rc<RefCell<Vec<Listener>>> = Rc::new(RefCell::new(Vec::new()));
        let held_in = held.clone();
        let build_panel = panel.clone();
        let items_weak = weak.clone();
        let pop = self.open_popover(
            "vela-callout-pop",
            label,
            PopPlacement {
                center: false,
                gap: 6.0,
            },
            Some(anchor.clone()),
            move |card| {
                let root = make(&doc, "div", "vela-callout-panel");
                attr(&root, "data-slot", "callout-bubble-panel");
                let _ = card.append_child(&root);
                let card = &root;
                if let Some(title) = build_panel.title.as_deref().filter(|t| !t.is_empty()) {
                    let t = make(&doc, "div", "vela-callout-title");
                    attr(&t, "data-slot", "callout-bubble-title");
                    t.set_text_content(Some(title));
                    let _ = card.append_child(&t);
                }
                let mut actions: Option<Element> = None;
                for item in &build_panel.items {
                    match item {
                        LegendCalloutPanelItem::Text { text } => {
                            actions = None;
                            let t = make(&doc, "div", "vela-callout-text");
                            attr(&t, "data-slot", "callout-bubble-text");
                            t.set_text_content(Some(text));
                            let _ = card.append_child(&t);
                        }
                        LegendCalloutPanelItem::Button {
                            label,
                            primary,
                            run,
                            ..
                        } => {
                            let row = actions.get_or_insert_with(|| {
                                let a = make(&doc, "div", "vela-callout-actions");
                                attr(&a, "data-slot", "callout-bubble-actions");
                                let _ = card.append_child(&a);
                                a
                            });
                            let b = make(
                                &doc,
                                "button",
                                if *primary {
                                    "vela-callout-btn vela-callout-btn-primary"
                                } else {
                                    "vela-callout-btn"
                                },
                            );
                            attr(&b, "type", "button");
                            attr(&b, "data-slot", "callout-bubble-button");
                            if *primary {
                                attr(&b, "data-primary", "");
                            }
                            b.set_text_content(Some(label));
                            let run = run.clone();
                            let closes = item.closes_panel();
                            let weak = items_weak.clone();
                            // The listener lives as long as the card (retired with it).
                            let l = on(&b, "click", move |_ev: JsValue| {
                                run();
                                if closes && let Some(ui) = weak.upgrade() {
                                    ui.close_callout_panel();
                                }
                            });
                            held_in.borrow_mut().push(l);
                            let _ = row.append_child(&b);
                        }
                    }
                }
            },
            dismiss,
        );
        let rect = self.rect_in_root(anchor);
        self.place_popover(&pop, rect);
        *self.callout_panel.borrow_mut() = Some(OpenCallout {
            pop,
            row_id: row_id.to_string(),
            anchor: anchor.clone(),
            _held: held,
        });
    }

    // ── building a row ──

    fn ctl_button(&self, class: &str, ctl: &str, label: &str, icon: &str) -> Element {
        let b = make(&self.doc, "button", class);
        attr(&b, "type", "button");
        attr(&b, "aria-label", label);
        attr(&b, "data-vela-ctl", ctl);
        b.set_inner_html(&icon_at(icon, ICON_PX));
        b
    }

    fn build_row(self: &Rc<Self>, spec: &LegendRow, pane_id: &str, seq: u64) -> Row {
        let id = spec.id.clone();
        let el = make(&self.doc, "div", "vela-ind-row");
        attr(&el, "role", "group");
        attr(&el, "tabindex", "0");
        attr(&el, "aria-label", &spec.title);
        attr(&el, "data-vela-id", &id);
        attr(&el, "data-open", "0");
        attr(&el, "data-hidden", "0");
        let title_wrap = make(&self.doc, "span", "vela-ind-title-wrap");
        let title_el = make(&self.doc, "span", "vela-ind-title");
        title_el.set_text_content(Some(&spec.title));
        let _ = title_wrap.append_child(&title_el);
        let callouts_el = make(&self.doc, "span", "vela-ind-callouts");
        let status_el = make(&self.doc, "span", "vela-ind-status");
        attr(&status_el, "data-status", "idle");
        attr(&status_el, "aria-hidden", "true");
        let values_el = make(&self.doc, "span", "vela-ind-values");
        let controls_el = make(&self.doc, "span", "vela-ind-controls");
        let eye = self.ctl_button("vela-ind-ctl vela-ind-eye", "eye", &self.lt("hide"), "eye");
        let gear = self.ctl_button("vela-ind-ctl", "settings", &self.lt("settings"), "gear");
        set_style(&gear, "display", "none"); // until the model says there is something to set
        let move_btn = self.ctl_button("vela-ind-ctl", "move", &self.lt("move"), "move");
        let extras_el = make(&self.doc, "span", "vela-ind-extras");
        let close_btn = self.ctl_button("vela-ind-close", "remove", &self.lt("remove"), "close");
        for c in [&eye, &gear, &move_btn, &extras_el, &close_btn] {
            let _ = controls_el.append_child(c);
        }
        for c in [
            &title_wrap,
            &callouts_el,
            &status_el,
            &values_el,
            &controls_el,
        ] {
            let _ = el.append_child(c);
        }

        let mut listeners: Vec<Listener> = Vec::new();
        let mut tips: Vec<Tip> = Vec::new();
        let w = self.weak();
        macro_rules! handler {
            ($target:expr, $event:expr, |$ui:ident, $ev:ident| $body:block) => {{
                let w = w.clone();
                listeners.push(on($target, $event, move |$ev: JsValue| {
                    let Some($ui) = w.upgrade() else { return };
                    $body
                }));
            }};
        }
        {
            let row_id = id.clone();
            handler!(&title_wrap, "mouseenter", |ui, _ev| {
                ui.set_highlighted(&row_id, true);
            });
        }
        {
            let row_id = id.clone();
            let row_el = el.clone();
            handler!(&el, "mouseleave", |ui, _ev| {
                let focus_inside = ui
                    .doc
                    .active_element()
                    .is_some_and(|a| vela_contains(&row_el, &JsValue::from(a)));
                if ui.legend.borrow().selected.as_deref() != Some(row_id.as_str()) && !focus_inside
                {
                    ui.set_highlighted(&row_id, false);
                }
            });
        }
        {
            let row_id = id.clone();
            handler!(&el, "focusin", |ui, _ev| {
                ui.set_highlighted(&row_id, true);
            });
        }
        {
            let row_id = id.clone();
            let row_el = el.clone();
            handler!(&el, "focusout", |ui, ev| {
                let to = get_prop(&ev, "relatedTarget");
                if !vela_contains(&row_el, &to)
                    && ui.legend.borrow().selected.as_deref() != Some(row_id.as_str())
                {
                    ui.set_highlighted(&row_id, false);
                }
            });
        }
        {
            let row_id = id.clone();
            let row_el = el.clone();
            handler!(&el, "click", |ui, ev| {
                if get_f64(&ev, "button").unwrap_or(0.0) != 0.0 || target_in_button(&ev, &row_el) {
                    return;
                }
                ui.select_row(&row_id);
            });
        }
        {
            let row_id = id.clone();
            let row_el = el.clone();
            handler!(&el, "dblclick", |ui, ev| {
                if target_in_button(&ev, &row_el) {
                    return;
                }
                ui.open_settings(&row_id);
            });
        }
        {
            let row_el = el.clone();
            handler!(&el, "keydown", |ui, ev| {
                let own = get_prop(&ev, "target") == JsValue::from(row_el.clone());
                match get_str(&ev, "key").as_str() {
                    "Enter" | " " if own => {
                        prevent_default(&ev);
                        let id = row_el.get_attribute("data-vela-id").unwrap_or_default();
                        ui.select_row(&id);
                    }
                    "Escape" => ui.clear_selection(),
                    _ => {}
                }
            });
        }
        handler!(&el, "mousedown", |_ui, ev| {
            if get_f64(&ev, "button") == Some(1.0) {
                prevent_default(&ev); // no middle-button autoscroll
            }
        });
        {
            let row_id = id.clone();
            handler!(&el, "auxclick", |ui, ev| {
                if get_f64(&ev, "button") != Some(1.0) {
                    return;
                }
                prevent_default(&ev);
                if let Some(r) = ui.renderer() {
                    r.notify_remove_indicator(&row_id);
                }
            });
        }
        {
            let row_id = id.clone();
            handler!(&el, "contextmenu", |ui, ev| {
                prevent_default(&ev);
                stop_propagation(&ev);
                let root = ui.root.get_bounding_client_rect();
                let x = get_f64(&ev, "clientX").unwrap_or(0.0) - root.left();
                let y = get_f64(&ev, "clientY").unwrap_or(0.0) - root.top();
                ui.open_row_menu(&row_id, x, y);
            });
        }
        {
            let row_id = id.clone();
            handler!(&eye, "click", |ui, _ev| {
                let hidden = ui
                    .legend
                    .borrow()
                    .rows
                    .get(&row_id)
                    .is_some_and(|r| r.hidden);
                if let Some(r) = ui.renderer() {
                    // Currently hidden: request show; else hide.
                    r.notify_toggle_visible(&row_id, hidden);
                }
            });
        }
        {
            let row_id = id.clone();
            handler!(&gear, "click", |ui, _ev| {
                ui.open_settings(&row_id);
            });
        }
        {
            let row_id = id.clone();
            let anchor = move_btn.clone();
            handler!(&move_btn, "click", |ui, ev| {
                stop_propagation(&ev);
                ui.open_move_menu(&row_id, &anchor);
            });
        }
        {
            let row_id = id.clone();
            handler!(&close_btn, "click", |ui, _ev| {
                if let Some(r) = ui.renderer() {
                    r.notify_remove_indicator(&row_id);
                }
            });
        }
        // Tips (the label is read live where it changes).
        {
            let weak = self.weak();
            let row_id = id.clone();
            tips.push(tooltip::attach(
                self,
                &eye,
                move || {
                    weak.upgrade()
                        .map(|ui| {
                            let hidden = ui
                                .legend
                                .borrow()
                                .rows
                                .get(&row_id)
                                .is_some_and(|r| r.hidden);
                            ui.lt(if hidden { "show" } else { "hide" })
                        })
                        .unwrap_or_default()
                },
                Placement::Below,
                false,
            ));
        }
        for (btn, key) in [
            (&gear, "settings"),
            (&move_btn, "move"),
            (&close_btn, "remove"),
        ] {
            let weak = self.weak();
            tips.push(tooltip::attach(
                self,
                btn,
                move || weak.upgrade().map(|ui| ui.lt(key)).unwrap_or_default(),
                Placement::Below,
                false,
            ));
        }
        self.ensure_doc_click();
        Row {
            id,
            seq,
            title: spec.title.clone(),
            pane_id: pane_id.to_string(),
            native: spec.native,
            hidden: false,
            highlighted: false,
            has_settings: false,
            show_values: None,
            plot_values: Vec::new(),
            plot_key: String::new(),
            el,
            title_el,
            status_el,
            callouts_el,
            values_el,
            extras_el,
            eye,
            gear,
            move_btn,
            close_btn,
            _listeners: listeners,
            tips,
            extras: Vec::new(),
            callouts: Vec::new(),
        }
    }

    /// A click anywhere outside the legend rows clears the selection outline.
    fn ensure_doc_click(self: &Rc<Self>) {
        if self.legend.borrow().doc_click.is_some() {
            return;
        }
        let weak = self.weak();
        let target: &web_sys::EventTarget = self.doc.as_ref();
        let l = Listener::new(target, "click", false, true, move |ev: JsValue| {
            let Some(ui) = weak.upgrade() else { return };
            let (has_selection, els): (bool, Vec<Element>) = {
                let st = ui.legend.borrow();
                (
                    st.selected.is_some(),
                    st.rows.values().map(|r| r.el.clone()).collect(),
                )
            };
            if !has_selection {
                return;
            }
            let target = get_prop(&ev, "target");
            if els.iter().any(|e| vela_contains(e, &target)) {
                return;
            }
            ui.clear_selection();
        });
        self.legend.borrow_mut().doc_click = Some(l);
    }

    fn open_settings(&self, id: &str) {
        let has = self
            .legend
            .borrow()
            .rows
            .get(id)
            .is_some_and(|r| r.has_settings);
        if !has {
            return;
        }
        if let Some(r) = self.renderer() {
            r.open_indicator_settings(id);
        }
    }

    // ── highlight / selection ──

    fn set_highlighted(&self, id: &str, on: bool) {
        let close_callout;
        {
            let doc = self.doc.clone();
            let mut st = self.legend.borrow_mut();
            let default_shown = st.values_visible;
            let Some(row) = st.rows.get_mut(id) else {
                return;
            };
            if row.highlighted == on {
                return;
            }
            row.highlighted = on;
            let shown = row.show_values.unwrap_or(default_shown);
            render_values(&doc, row, shown); // values step aside while the controls are out
            close_callout = sync_actions(row);
        }
        if close_callout
            && self
                .callout_panel
                .borrow()
                .as_ref()
                .is_some_and(|c| c.row_id == id)
        {
            self.close_callout_panel();
        }
    }

    fn select_row(&self, id: &str) {
        if self.legend.borrow().selected.as_deref() == Some(id) {
            return;
        }
        self.clear_selection();
        self.legend.borrow_mut().selected = Some(id.to_string());
        self.set_highlighted(id, true);
    }

    pub(crate) fn clear_selection(&self) {
        let prev = self.legend.borrow_mut().selected.take();
        if let Some(prev) = prev {
            self.set_highlighted(&prev, false);
        }
    }

    // ── fold toggle ──

    /// Keep the fold chip in step with the indicator count: a chevron under the price-pane
    /// rows with 2+ indicators (mobile and an overview override earn it from the first),
    /// the bordered "count" chip once folded.
    pub(crate) fn sync_fold_toggle(self: &Rc<Self>) {
        let mut st = self.legend.borrow_mut();
        let count = st.rows.len();
        let min_rows = if st.overview || st.mobile { 1 } else { 2 };
        if count < min_rows {
            let was_folded = st.folded;
            // Never strand a surviving row hidden behind a toggle that just disappeared.
            if count > 0 {
                st.folded = false;
            }
            if let Some(fold) = st.fold.take() {
                fold.el.remove();
                self.retire(Box::new(fold));
            }
            drop(st);
            if was_folded && count > 0 {
                self.schedule(D_LAYOUT);
            }
            return;
        }
        if st.fold.is_none() {
            let el = make(&self.doc, "button", "vela-ind-fold");
            attr(&el, "type", "button");
            let weak = self.weak();
            let listener = on(&el, "click", move |_ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                let action = if ui.legend.borrow().overview {
                    ui.renderer().and_then(|r| r.legend_overview_action())
                } else {
                    None
                };
                if let Some(action) = action {
                    action();
                    return;
                }
                {
                    let mut st = ui.legend.borrow_mut();
                    st.folded = !st.folded;
                }
                ui.sync_fold_toggle();
            });
            let weak = self.weak();
            let tip = tooltip::attach(
                self,
                &el,
                move || {
                    weak.upgrade()
                        .map(|ui| ui.fold_label(&ui.legend.borrow()))
                        .unwrap_or_default()
                },
                Placement::Below,
                false,
            );
            st.fold = Some(Fold {
                el,
                _listener: listener,
                _tip: tip,
            });
        }
        let label = self.fold_label(&st);
        let folded = st.folded;
        let overview = st.overview;
        let fold_el = st.fold.as_ref().map(|f| f.el.clone()).expect("fold exists");
        attr(&fold_el, "aria-label", &label);
        if overview {
            let _ = fold_el.remove_attribute("aria-expanded");
        } else {
            attr(
                &fold_el,
                "aria-expanded",
                if folded { "false" } else { "true" },
            );
        }
        fold_el.set_text_content(Some(""));
        let icon = make(&self.doc, "span", "vela-ind-fold-icon");
        attr(&icon, "aria-hidden", "true");
        icon.set_inner_html(&icon_at(
            if overview {
                "objects"
            } else if folded {
                "chevron-down"
            } else {
                "chevron-up"
            },
            ICON_PX,
        ));
        let _ = fold_el.append_child(&icon);
        if folded {
            let n = make(&self.doc, "span", "vela-ind-fold-count");
            n.set_text_content(Some(&count.to_string()));
            let _ = fold_el.append_child(&n);
        }
        // The toggle lives on the PRICE pane's column (created on demand), last.
        let lg = self.legend_for(&mut st, "price");
        let _ = lg.append_child(&fold_el);
        drop(st);
        self.schedule(D_LAYOUT);
    }

    // ── menus ──

    fn row_menu_items(&self, id: &str) -> Vec<MenuItem> {
        let mut items = self.stack_items(id);
        let st = self.legend.borrow();
        if let Some(row) = st.rows.get(id) {
            items.push(MenuItem {
                id: "values".to_string(),
                label: self.lt("values"),
                checked: Some(st.row_shows_values(row)),
                separator_before: !items.is_empty(),
            });
        }
        items
    }

    /// A row's one-step restacking entries (forward, then back), for whichever neighbor
    /// exists.
    fn stack_items(&self, id: &str) -> Vec<MenuItem> {
        let Some(r) = self.renderer() else {
            return Vec::new();
        };
        let name_of = |n: &str| {
            self.legend
                .borrow()
                .rows
                .get(n)
                .map(|row| row.title.clone())
        };
        let mut items = Vec::new();
        let label = |neighbor: Option<Neighbor>, price: &str, named: &str, unnamed: &str| {
            neighbor.map(|n| match n {
                Neighbor::Price => self.lt(price),
                Neighbor::Indicator(other) => match name_of(&other) {
                    Some(name) => self.lt_with(named, &name),
                    None => self.lt(unnamed),
                },
            })
        };
        if let Some(l) = label(r.layer_above(id), "frontPrice", "front", "frontNext") {
            items.push(MenuItem {
                id: STACK_RAISE.to_string(),
                label: l,
                checked: None,
                separator_before: false,
            });
        }
        if let Some(l) = label(r.layer_below(id), "backPrice", "back", "backNext") {
            items.push(MenuItem {
                id: STACK_LOWER.to_string(),
                label: l,
                checked: None,
                separator_before: false,
            });
        }
        items
    }

    fn open_row_menu(self: &Rc<Self>, id: &str, x: f64, y: f64) {
        let items = self.row_menu_items(id);
        let weak = self.weak();
        let row_id = id.to_string();
        let label = self.lt("indicators");
        let pick: Rc<dyn Fn(&str)> = Rc::new(move |item: &str| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.run_stack_item(item, &row_id) || item != "values" {
                return;
            }
            let doc = ui.doc.clone();
            let mut st = ui.legend.borrow_mut();
            let default_shown = st.values_visible;
            if let Some(row) = st.rows.get_mut(&row_id) {
                let shown = !row.show_values.unwrap_or(default_shown);
                row.show_values = Some(shown);
                render_values(&doc, row, shown);
            }
        });
        self.open_menu(&items, x, y, &label, None, pick);
    }

    fn run_stack_item(&self, item: &str, id: &str) -> bool {
        let Some(r) = self.renderer() else {
            return false;
        };
        match item {
            STACK_RAISE => r.raise_overlay(id),
            STACK_LOWER => r.lower_overlay(id),
            _ => return false,
        }
        true
    }

    /// The row's "move to" menu under its button: restacking first, then the panes.
    fn open_move_menu(self: &Rc<Self>, id: &str, anchor: &Element) {
        let Some(r) = self.renderer() else { return };
        let (current, rows_in_pane) = {
            let st = self.legend.borrow();
            let cur = st
                .rows
                .get(id)
                .map_or_else(|| "price".to_string(), |row| row.pane_id.clone());
            let n = st.rows.values().filter(|row| row.pane_id == cur).count();
            (cur, n)
        };
        let views: Vec<PaneView> = r.pane_views();
        let mut entries: Vec<(String, MoveTarget)> = Vec::new();
        for p in &views {
            if p.id == current {
                continue;
            }
            if p.kind == PaneKind::Price {
                entries.push((self.lt("toMain"), MoveTarget::Price));
            } else {
                // A study pane is named after its master indicator; without one, the
                // positional "Panel {n}".
                let name = r.pane_master_title(&p.id).unwrap_or_else(|| {
                    self.text_with(
                        &msg("chart.pane.numbered", "Panel {n}"),
                        &[("n", MessageArg::from(p.index))],
                    )
                });
                entries.push((
                    self.lt_with("toPane", &name),
                    MoveTarget::Pane(p.id.clone()),
                ));
            }
        }
        // Offer only moves that change something: the sole indicator of a study pane owns it
        // already, and panes never sit above the price pane.
        let sole_in_study = current != "price" && rows_in_pane <= 1;
        if !sole_in_study {
            if current != "price" {
                entries.push((
                    self.lt("newAbove"),
                    MoveTarget::NewPane(NewPanePlacement {
                        before: Some(current.clone()),
                        after: None,
                    }),
                ));
            }
            entries.push((
                self.lt("newBelow"),
                MoveTarget::NewPane(NewPanePlacement {
                    before: None,
                    after: Some(current.clone()),
                }),
            ));
        }
        let mut items = self.stack_items(id);
        let had_stack = !items.is_empty();
        for (i, (label, _)) in entries.iter().enumerate() {
            items.push(MenuItem {
                id: i.to_string(),
                label: label.clone(),
                checked: None,
                separator_before: i == 0 && had_stack,
            });
        }
        let targets: Vec<MoveTarget> = entries.into_iter().map(|(_, t)| t).collect();
        let weak = self.weak();
        let row_id = id.to_string();
        let pick: Rc<dyn Fn(&str)> = Rc::new(move |item: &str| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.run_stack_item(item, &row_id) {
                return;
            }
            if let Some(target) = item.parse::<usize>().ok().and_then(|i| targets.get(i))
                && let Some(r) = ui.renderer()
            {
                r.notify_move_indicator(&row_id, target);
            }
        });
        let (ax, ay, _aw, ah) = self.rect_in_root(anchor);
        self.open_menu(
            &items,
            ax,
            ay + ah + 4.0,
            &self.lt("move"),
            Some(anchor.clone()),
            pick,
        );
    }

    // ── positioning ──

    /// Pin each pane's column under the pane's top edge, hide the columns of panes with no
    /// room, apply the collapsed-strip and fold rules to the rows.
    pub(crate) fn legend_reposition(&self, views: &[PaneView], axis_w: f64) {
        let st = self.legend.borrow();
        for (pane, lg) in &st.containers {
            let bounds = views
                .iter()
                .find(|p| &p.id == pane)
                .map(|p| (p.top, p.height));
            let (top, height) = bounds.unwrap_or((0.0, 0.0));
            if top == 0.0 && height > 0.0 {
                attr(lg, "data-vela-pane-at-top", "");
            } else {
                let _ = lg.remove_attribute("data-vela-pane-at-top");
            }
            // A pane hidden by a maximize elsewhere collapses to ~0 height; titles switched
            // off hide every column.
            set_style(
                lg,
                "display",
                if height < 4.0 || !st.titles_visible {
                    "none"
                } else {
                    "flex"
                },
            );
            set_style(
                lg,
                "max-width",
                &format!("calc(100% - {}px)", axis_w + 10.0 + 6.0),
            );
            let collapsed = st.collapse.contains_key(pane);
            let master = st.collapse.get(pane).cloned().flatten();
            for row in st.rows.values().filter(|r| &r.pane_id == pane) {
                let hide = if collapsed {
                    Some(row.id.as_str()) != master.as_deref()
                } else {
                    st.folded
                };
                set_style(&row.el, "display", if hide { "none" } else { "flex" });
            }
            // A collapsed strip is too short for the usual inset: center its master row.
            let mut y = top + 8.0;
            if collapsed && height.is_finite() {
                let row_h = match f64::from(lg.unchecked_ref::<HtmlElement>().offset_height()) {
                    h if h > 0.0 => h,
                    _ => 20.0,
                };
                y = top + ((height - row_h) / 2.0).round().max(1.0);
            }
            set_style(lg, "top", &format!("{y}px"));
        }
    }
}
