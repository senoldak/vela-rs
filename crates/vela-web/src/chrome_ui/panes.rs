//! The per-pane hover cluster (`PaneControls.ts`) and the A / L buttons under each price
//! scale (`AxisScaleButtons.ts`).
//!
//! Pane cluster: study panes get move up / move down / collapse / maximize, the price pane
//! maximize only (and only when there is more than one pane). It shows while the pointer is
//! inside the pane; a collapsed pane keeps its expand chip and a maximized pane its restore
//! chip. The original builds a cluster of `display:none` buttons and reaches it by hover
//! only; here a hidden cluster stays in the tab order at zero opacity and reveals on focus,
//! so the controls are reachable from the keyboard. Clusters are rebuilt only when a pane's
//! state signature changes (a separator drag lays out on every move).
//!
//! Axis buttons: one cluster pinned to the bottom of the hovered pane's master scale column;
//! an active mode wears the inverse chip. Without hover it rests invisibly on the price pane
//! (keyboard reachable). Mouse only: touch has no hover and a drag-rescale must not sprout
//! buttons.

use std::collections::HashMap;
use std::rc::Rc;

use vela_core::i18n::msg;
use vela_core::model::PaneKind;
use vela_core::ports::chart_renderer::PaneMoveDir;
use vela_core::render::chrome::axis_layout::AXIS_MASTER_W;
use vela_core::render::native::PaneView;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::ChromeUi;
use super::icons::icon_at;
use super::tooltip::{self, Placement, Tip};
use crate::dom::{Listener, attr, make, on, set_style, set_styles, stop_propagation, vela_focus};

const ICON_PX: u32 = 12;
const AXIS_BORDER: f64 = 1.0;
const BTN_PX: f64 = 18.0;
const PAD_TOP: f64 = 8.0;
const PAD_BOTTOM: f64 = 6.0;
const CLUSTER_H: f64 = BTN_PX + PAD_TOP + PAD_BOTTOM;
/// Panes shorter than this skip the A / L buttons.
const MIN_PANE_H: f64 = 48.0;

struct Cluster {
    el: Element,
    sig: String,
    listeners: Vec<Listener>,
}

#[derive(Default)]
pub struct PaneControls {
    clusters: HashMap<String, Cluster>,
    views: Vec<PaneView>,
    axis_w: f64,
    hover: Option<String>,
    suspended: bool,
}

impl PaneControls {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Copy)]
enum Act {
    Move(PaneMoveDir),
    Collapse,
    Maximize,
}

fn signature(p: &PaneView) -> String {
    format!(
        "{:?}|{}|{}|{}|{}",
        p.kind, p.collapsed, p.maximized, p.index, p.count
    )
}

impl ChromeUi {
    // ── pane controls ──

    fn pane_text(&self, key: &str) -> String {
        let m = match key {
            "up" => msg("chart.paneControls.moveUp", "Move panel up"),
            "down" => msg("chart.paneControls.moveDown", "Move panel down"),
            "expand" => msg("chart.paneControls.expand", "Expand panel"),
            "collapse" => msg("chart.paneControls.collapse", "Collapse panel"),
            "restore" => msg("chart.paneControls.restore", "Restore panel"),
            _ => msg("chart.paneControls.maximize", "Maximize panel"),
        };
        self.text(&m)
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_button(
        self: &Rc<Self>,
        listeners: &mut Vec<Listener>,
        icon: &str,
        title: &str,
        disabled: bool,
        selected: bool,
        role: &str,
        pane_id: &str,
        act: Act,
    ) -> Element {
        let b = make(
            &self.doc,
            "button",
            if selected {
                "vela-pc-btn vela-pc-on"
            } else {
                "vela-pc-btn"
            },
        );
        attr(&b, "type", "button");
        attr(&b, "title", title);
        attr(&b, "aria-label", title);
        attr(&b, "data-role", role);
        b.set_inner_html(&icon_at(icon, ICON_PX));
        set_style(&b, "cursor", if disabled { "default" } else { "pointer" });
        set_style(&b, "opacity", if disabled { "0.3" } else { "1" });
        if disabled {
            attr(&b, "disabled", "");
        } else {
            let weak = self.weak();
            let id = pane_id.to_string();
            listeners.push(on(&b, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                let Some(r) = weak.upgrade().and_then(|ui| ui.renderer()) else {
                    return;
                };
                match act {
                    Act::Move(dir) => r.move_pane(&id, dir),
                    Act::Collapse => r.toggle_pane_collapse(&id),
                    Act::Maximize => r.toggle_pane_maximize(&id),
                }
            }));
        }
        b
    }

    fn build_cluster_buttons(self: &Rc<Self>, el: &Element, p: &PaneView) -> Vec<Listener> {
        // Keep keyboard focus on the same role across the rebuild.
        let focused_role = self.doc.active_element().and_then(|a| {
            crate::dom::vela_contains(el, &JsValue::from(a.clone()))
                .then(|| a.get_attribute("data-role"))
                .flatten()
        });
        el.set_text_content(Some(""));
        let mut listeners = Vec::new();
        let mut made: Vec<Element> = Vec::new();
        if p.kind == PaneKind::Study {
            made.push(self.pane_button(
                &mut listeners,
                "chevron-up",
                &self.pane_text("up"),
                p.index <= 1,
                false,
                "up",
                &p.id,
                Act::Move(PaneMoveDir::Up),
            ));
            made.push(self.pane_button(
                &mut listeners,
                "chevron-down",
                &self.pane_text("down"),
                p.index + 1 >= p.count,
                false,
                "down",
                &p.id,
                Act::Move(PaneMoveDir::Down),
            ));
            made.push(self.pane_button(
                &mut listeners,
                if p.collapsed {
                    "pane-expand"
                } else {
                    "pane-collapse"
                },
                &self.pane_text(if p.collapsed { "expand" } else { "collapse" }),
                false,
                p.collapsed,
                "collapse",
                &p.id,
                Act::Collapse,
            ));
        }
        // Maximize only makes sense with more than one pane to trade space between.
        if p.count > 1 {
            made.push(self.pane_button(
                &mut listeners,
                if p.maximized { "restore" } else { "maximize" },
                &self.pane_text(if p.maximized { "restore" } else { "maximize" }),
                false,
                p.maximized,
                "maximize",
                &p.id,
                Act::Maximize,
            ));
        }
        for b in &made {
            let _ = el.append_child(b);
        }
        if let Some(role) = focused_role
            && let Some(b) = made
                .iter()
                .find(|b| b.get_attribute("data-role").as_deref() == Some(role.as_str()))
        {
            vela_focus(b);
        }
        listeners
    }

    /// Rebuild the clusters whose pane state changed and re-place them all.
    pub(crate) fn panes_refresh(self: &Rc<Self>, views: &[PaneView], axis_w: f64) {
        let mut stale_listeners: Vec<Listener> = Vec::new();
        for p in views {
            let sig = signature(p);
            let existing = self
                .panes
                .borrow()
                .clusters
                .get(&p.id)
                .map(|c| (c.el.clone(), c.sig.clone()));
            match existing {
                Some((_, ref old)) if *old == sig => {}
                Some((el, _)) => {
                    let listeners = self.build_cluster_buttons(&el, p);
                    let mut st = self.panes.borrow_mut();
                    if let Some(c) = st.clusters.get_mut(&p.id) {
                        c.sig = sig;
                        stale_listeners.extend(std::mem::replace(&mut c.listeners, listeners));
                    }
                }
                None => {
                    let el = make(&self.doc, "div", "vela-pc");
                    attr(&el, "role", "group");
                    attr(&el, "aria-label", &p.id);
                    let _ = self.root.append_child(&el);
                    let listeners = self.build_cluster_buttons(&el, p);
                    self.panes
                        .borrow_mut()
                        .clusters
                        .insert(p.id.clone(), Cluster { el, sig, listeners });
                }
            }
        }
        let gone: Vec<String> = self
            .panes
            .borrow()
            .clusters
            .keys()
            .filter(|id| !views.iter().any(|p| &p.id == *id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(c) = self.panes.borrow_mut().clusters.remove(&id) {
                c.el.remove();
                stale_listeners.extend(c.listeners);
            }
        }
        if !stale_listeners.is_empty() {
            self.retire(Box::new(stale_listeners));
        }
        {
            let mut st = self.panes.borrow_mut();
            st.views = views.to_vec();
            st.axis_w = axis_w;
        }
        self.panes_reposition();
    }

    pub(crate) fn panes_reposition(&self) {
        let st = self.panes.borrow();
        let right = st.axis_w + 6.0;
        for (id, cluster) in &st.clusters {
            let Some(p) = st.views.iter().find(|p| &p.id == id) else {
                set_style(&cluster.el, "display", "none");
                continue;
            };
            let hovered = st.hover.as_deref() == Some(id.as_str());
            let has_buttons = p.kind == PaneKind::Study || p.count > 1;
            // STATE chips stay visible without hover: a collapsed pane's expand chip (always)
            // and a maximized pane's restore chip (desktop only).
            let state_chip = if p.collapsed {
                Some("collapse")
            } else if !st.suspended && p.maximized {
                Some("maximize")
            } else {
                None
            };
            let shown = has_buttons && p.height > 8.0;
            set_style(&cluster.el, "right", &format!("{right}px"));
            let top = if p.collapsed {
                p.top + ((p.height - 24.0) / 2.0).round().max(1.0)
            } else {
                p.top + 4.0
            };
            set_style(&cluster.el, "top", &format!("{top}px"));
            if !shown {
                set_style(&cluster.el, "display", "none");
                continue;
            }
            set_style(&cluster.el, "display", "flex");
            let revealed = !st.suspended && hovered || state_chip.is_some();
            attr(&cluster.el, "data-rest", if revealed { "0" } else { "1" });
            let solo = state_chip.is_some() && !hovered;
            set_style(
                &cluster.el,
                "background",
                if solo {
                    "transparent"
                } else {
                    "rgba(0,0,0,0.65)"
                },
            );
            // Siblings of a solo chip stay laid out but invisible, keeping the chip's slot.
            let mut child = cluster.el.first_element_child();
            while let Some(c) = child {
                let role = c.get_attribute("data-role");
                let hide = solo && role.as_deref() != state_chip;
                set_style(&c, "visibility", if hide { "hidden" } else { "visible" });
                child = c.next_element_sibling();
            }
        }
    }

    /// The pointer is at plot-relative `y`: reveal the cluster of the pane under it.
    pub(crate) fn panes_hover(self: &Rc<Self>, views: &[PaneView], axis_w: f64, y: f64) {
        let hit = views
            .iter()
            .find(|p| y >= p.top && y <= p.top + p.height)
            .map(|p| p.id.clone());
        let changed = {
            let mut st = self.panes.borrow_mut();
            st.views = views.to_vec();
            st.axis_w = axis_w;
            if st.suspended || st.hover == hit {
                false
            } else {
                st.hover = hit;
                true
            }
        };
        if changed {
            self.panes_reposition();
        }
    }

    pub(crate) fn panes_leave(&self) {
        let changed = self.panes.borrow_mut().hover.take().is_some();
        if changed {
            self.panes_reposition();
        }
    }

    pub(crate) fn panes_set_suspended(&self, on: bool) {
        {
            let mut st = self.panes.borrow_mut();
            if st.suspended == on {
                return;
            }
            st.suspended = on;
            if on {
                st.hover = None;
            }
        }
        self.panes_reposition();
    }

    pub(crate) fn panes_relabel(self: &Rc<Self>) {
        let (views, axis_w) = {
            let mut st = self.panes.borrow_mut();
            for c in st.clusters.values_mut() {
                c.sig.clear();
            }
            (st.views.clone(), st.axis_w)
        };
        self.panes_refresh(&views, axis_w);
    }

    pub(crate) fn panes_dispose(&self) {
        let mut st = self.panes.borrow_mut();
        for (_, c) in st.clusters.drain() {
            c.el.remove();
        }
        drop(st);
        if let Some(a) = self.axis.borrow_mut().el.take() {
            a.remove();
        }
        let mut axis = self.axis.borrow_mut();
        axis.tips.clear();
        axis.listeners.clear();
    }

    // ── axis scale buttons ──

    fn axis_text(&self, key: &str) -> String {
        self.text(&match key {
            "auto" => msg("chart.axisButtons.auto", "Auto (fits data to screen)"),
            "log" => msg("chart.axisButtons.log", "Logarithmic scale"),
            "autoLetter" => msg("chart.axisButtons.autoLetter", "A"),
            _ => msg("chart.axisButtons.logLetter", "L"),
        })
    }

    pub(crate) fn build_axis_buttons(self: &Rc<Self>) {
        let el = make(&self.doc, "div", "vela-axis");
        attr(&el, "role", "group");
        attr(&el, "aria-label", &self.axis_text("auto"));
        set_style(&el, "width", &format!("{}px", AXIS_MASTER_W - AXIS_BORDER));
        let mut listeners = Vec::new();
        let mut tips = Vec::new();
        let mut buttons = Vec::new();
        for (letter, title, is_auto) in [("autoLetter", "auto", true), ("logLetter", "log", false)]
        {
            let b = make(&self.doc, "button", "vela-axis-btn");
            attr(&b, "type", "button");
            attr(&b, "aria-label", &self.axis_text(title));
            attr(&b, "aria-pressed", "false");
            attr(&b, "data-role", if is_auto { "auto" } else { "log" });
            b.set_text_content(Some(&self.axis_text(letter)));
            let weak = self.weak();
            tips.push(tooltip::attach(
                self,
                &b,
                {
                    let weak = weak.clone();
                    move || {
                        weak.upgrade()
                            .map(|ui| ui.axis_text(title))
                            .unwrap_or_default()
                    }
                },
                Placement::Above,
                false,
            ));
            listeners.push(on(&b, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                let Some(ui) = weak.upgrade() else { return };
                let Some(r) = ui.renderer() else { return };
                let target = ui.axis.borrow().target.clone();
                if let Some(id) = target {
                    if is_auto {
                        r.toggle_pane_auto(&id);
                    } else {
                        r.toggle_pane_log(&id);
                    }
                    ui.axis_sync(&r.pane_views());
                }
            }));
            let _ = el.append_child(&b);
            buttons.push(b);
        }
        let _ = self.root.append_child(&el);
        let mut axis = self.axis.borrow_mut();
        axis.el = Some(el);
        axis.buttons = buttons;
        axis.listeners = listeners;
        axis.tips = tips;
    }

    pub(crate) fn axis_relabel(&self) {
        let axis = self.axis.borrow();
        for (b, (letter, title)) in axis
            .buttons
            .iter()
            .zip([("autoLetter", "auto"), ("logLetter", "log")])
        {
            attr(b, "aria-label", &self.axis_text(title));
            b.set_text_content(Some(&self.axis_text(letter)));
        }
    }

    fn axis_eligible(p: &PaneView) -> bool {
        !p.collapsed && p.height >= MIN_PANE_H
    }

    /// Reflect the target pane's auto / log state on the buttons.
    fn axis_sync(&self, views: &[PaneView]) {
        let axis = self.axis.borrow();
        let Some(id) = axis.target.as_deref() else {
            return;
        };
        let Some(p) = views.iter().find(|p| p.id == id) else {
            return;
        };
        for (b, on) in axis.buttons.iter().zip([p.auto, p.log]) {
            b.set_class_name(if on {
                "vela-axis-btn vela-axis-on"
            } else {
                "vela-axis-btn"
            });
            attr(b, "aria-pressed", if on { "true" } else { "false" });
        }
    }

    pub(crate) fn axis_hover(
        self: &Rc<Self>,
        views: &[PaneView],
        axis_w: f64,
        x: f64,
        y: f64,
        plot_w: f64,
    ) {
        let gutter_left = plot_w - axis_w;
        let mut hit = None;
        if x >= gutter_left && x <= gutter_left + AXIS_MASTER_W {
            hit = views
                .iter()
                .find(|p| Self::axis_eligible(p) && y >= p.top && y <= p.top + p.height)
                .map(|p| p.id.clone());
        }
        let changed = {
            let mut axis = self.axis.borrow_mut();
            let changed = axis.hover != hit;
            axis.hover = hit.clone();
            changed
        };
        if changed {
            self.axis_reposition(views, axis_w);
        } else if hit.is_some() {
            self.axis_sync(views); // scale state can change under a still cursor
        }
    }

    pub(crate) fn axis_leave(&self) {
        let changed = self.axis.borrow_mut().hover.take().is_some();
        if changed && let Some(r) = self.renderer() {
            self.axis_reposition(&r.pane_views(), r.right_axis_width());
        }
    }

    /// Re-place (or hide) the cluster after a hover or layout change.
    pub(crate) fn axis_reposition(&self, views: &[PaneView], axis_w: f64) {
        let (hover, el) = {
            let axis = self.axis.borrow();
            (axis.hover.clone(), axis.el.clone())
        };
        let Some(el) = el else { return };
        let hovered = hover
            .as_ref()
            .and_then(|id| views.iter().find(|p| &p.id == id))
            .filter(|p| Self::axis_eligible(p));
        let rest = views
            .iter()
            .find(|p| p.kind == PaneKind::Price)
            .filter(|p| Self::axis_eligible(p));
        let (pane, resting) = match (hovered, rest) {
            (Some(p), _) => (p, false),
            (None, Some(p)) => (p, true),
            _ => {
                set_style(&el, "display", "none");
                attr(&el, "data-rest", "0");
                self.axis.borrow_mut().target = None;
                return;
            }
        };
        set_styles(
            &el,
            &[
                ("right", &format!("{}px", axis_w - AXIS_MASTER_W)),
                ("top", &format!("{}px", pane.top + pane.height - CLUSTER_H)),
                ("display", "flex"),
            ],
        );
        attr(&el, "data-rest", if resting { "1" } else { "0" });
        self.axis.borrow_mut().target = Some(pane.id.clone());
        self.axis_sync(views);
    }
}

#[derive(Default)]
pub struct AxisButtons {
    el: Option<Element>,
    buttons: Vec<Element>,
    listeners: Vec<Listener>,
    tips: Vec<Tip>,
    hover: Option<String>,
    target: Option<String>,
}

impl AxisButtons {
    pub fn new() -> Self {
        Self::default()
    }
}
