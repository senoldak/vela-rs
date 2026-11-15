//! The docked drawing toolbar: a port of the original's `DrawingToolbar.ts`, minus the
//! `vela-ui` kit.
//!
//! The controller pushes a full [`ToolbarUiState`] snapshot after every change; this widget
//! renders it. A group cell's icon arms the group's last-used tool and its chevron opens a
//! flyout listing the group's tools (sections, shortcut hints, a favorite star, a check on
//! the armed one); the magnet cell's chevron opens an Off / Weak / Strong menu; measure,
//! eraser, stay-in-drawing-mode and the collapse toggle are plain buttons. User actions go
//! back through the controller's `toolbar_*` methods.
//!
//! Differences from the original:
//!
//! - Tooltips are the browser's native `title` (the original draws its own 700 ms-dwell
//!   chrome tooltip, from the kit); every button also has an `aria-label`.
//! - Fixed labels (Cursor, Measure, Magnet snap, ...) read through the chart's translator
//!   (`drawing.toolbar.*`), as the tool and group labels do.
//! - No drawings-sync toggle (a multi-chart host's concern, absent without the callback).
//! - Colors come from the `--vela-chrome-*` properties instead of the kit's tokens.
//! - Keyboard: every control is a `<button>` (Enter / Space activate); a flyout opened from
//!   the keyboard takes focus, ArrowUp / ArrowDown move through it and Escape closes it.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

use serde_json::Value;
use vela_core::drawings::SnapMode;
use vela_core::drawings::toolbar::{ToolGroup, ToolSection, ToolbarDefinition};
use vela_core::render::drawings::ToolbarUiState;
use vela_core::render::drawings::user_controller::{TOOLBAR_COLLAPSED_WIDTH, TOOLBAR_WIDTH};
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element};

use super::DrawingsUi;
use super::style::{arrow_hit_html, hit_html, sized_icon};
use crate::dialogs::Tr;
use crate::dom::{
    Listener, attr, get_f64, get_prop, get_str, make, on, prevent_default, set_style, set_styles,
    stop_propagation, vela_contains, vela_focus,
};
use crate::icons::icon;
use vela_core::i18n::{MessageArg, msg};

/// Space between the bar's edge and an open flyout.
const FLYOUT_GAP: f64 = 3.0;

/// The fixed labels (the original's `TEXT`): message id and English fallback.
mod text {
    pub type M = (&'static str, &'static str);
    pub const CURSOR: M = ("drawing.toolbar.cursor", "Cursor");
    pub const MEASURE: M = ("drawing.toolbar.measure", "Measure");
    pub const ERASER: M = ("drawing.toolbar.eraser", "Eraser (click/drag to delete)");
    pub const STAY: M = ("drawing.toolbar.stay", "Stay in drawing mode");
    pub const COLLAPSE: M = ("drawing.toolbar.collapse", "Collapse toolbar");
    pub const EXPAND: M = ("drawing.toolbar.expand", "Expand toolbar");
    pub const REMOVE_FAVORITE: M = ("drawing.toolbar.removeFavorite", "Remove from favorites");
    pub const ADD_FAVORITE: M = ("drawing.toolbar.addFavorite", "Add to favorites");
    pub const MAGNET_SNAP: M = ("drawing.toolbar.magnetSnap", "Magnet snap");
    pub const MAGNET_OFF: M = ("drawing.toolbar.magnetSnapOff", "Magnet snap: Off");
    pub const MAGNET_WEAK: M = ("drawing.toolbar.magnetSnapWeak", "Magnet snap: Weak");
    pub const MAGNET_STRONG: M = ("drawing.toolbar.magnetSnapStrong", "Magnet snap: Strong");
    pub const MAGNET: M = ("drawing.toolbar.magnet", "Magnet");
    pub const OFF: M = ("drawing.toolbar.magnet.off", "Off");
    pub const WEAK: M = ("drawing.toolbar.magnet.weak", "Weak");
    pub const STRONG: M = ("drawing.toolbar.magnet.strong", "Strong");
}

fn more_label(tr: &Tr, tool: &str) -> String {
    tr.mv(
        &msg("drawing.toolbar.more", "{tool} \u{2014} more"),
        &[("tool", MessageArg::Text(tool.to_string().into()))],
    )
}

/// A fixed label through the chart's translator.
fn tx(tr: &Tr, m: text::M) -> String {
    tr.t(m.0, m.1)
}

/// What the last snapshot said, plus the widget's own state (last-used tool per group,
/// collapsed).
struct St {
    built: bool,
    raw: Value,
    def: ToolbarDefinition,
    last_used: HashMap<String, String>,
    active: Option<String>,
    favorites: Vec<String>,
    shortcuts: BTreeMap<String, String>,
    magnet: SnapMode,
    /// The strength the magnet icon restores when it toggles back on.
    last_magnet_on: SnapMode,
    stay: bool,
    measure: bool,
    eraser: bool,
    collapsed: bool,
    visible: bool,
}

struct GroupEls {
    id: String,
    cell: Element,
    icon: Element,
    arrow: Option<Element>,
    /// The tool type whose icon the button shows now.
    shown: String,
}

#[derive(Default)]
struct Els {
    cursor: Option<Element>,
    groups: Vec<GroupEls>,
    measure: Option<Element>,
    eraser: Option<Element>,
    magnet_cell: Option<Element>,
    magnet_icon: Option<Element>,
    magnet_arrow: Option<Element>,
    stay: Option<Element>,
    collapse: Option<Element>,
}

/// An open flyout.
struct Flyout {
    owner: String,
    el: Element,
    cell: Element,
    /// The chevron that opened it (focus returns there on Escape).
    opener: Element,
    items: Vec<Element>,
    stars: Vec<(String, Element)>,
    listeners: Vec<Listener>,
    outside: Listener,
}

pub struct Toolbar {
    ui: Weak<DrawingsUi>,
    me: Weak<Toolbar>,
    doc: Document,
    host: Element,
    root: Element,
    st: RefCell<St>,
    els: RefCell<Els>,
    fly: RefCell<Option<Flyout>>,
    listeners: RefCell<Vec<Listener>>,
}

const MAGNET_ID: &str = "__magnet__";

impl DrawingsUi {
    pub(super) fn toolbar_event(&self, state: &ToolbarUiState) {
        let toolbar = {
            let mut slot = self.toolbar.borrow_mut();
            if slot.is_none() {
                *slot = Some(Toolbar::new(self, &self.doc, &self.wrapper));
            }
            slot.clone()
        };
        if let Some(toolbar) = toolbar {
            toolbar.apply(state);
        }
    }
}

impl Toolbar {
    fn new(ui: &DrawingsUi, doc: &Document, host: &Element) -> Rc<Self> {
        let root = make(doc, "div", "vela-dtb");
        attr(&root, "role", "toolbar");
        attr(&root, "aria-orientation", "vertical");
        let toolbar = Rc::new_cyclic(|me| Toolbar {
            ui: ui.weak(),
            me: me.clone(),
            doc: doc.clone(),
            host: host.clone(),
            root: root.clone(),
            st: RefCell::new(St {
                built: false,
                raw: Value::Null,
                def: ToolbarDefinition::default(),
                last_used: HashMap::new(),
                active: None,
                favorites: Vec::new(),
                shortcuts: BTreeMap::new(),
                magnet: SnapMode::Off,
                last_magnet_on: SnapMode::Weak,
                stay: false,
                measure: false,
                eraser: false,
                collapsed: false,
                visible: false,
            }),
            els: RefCell::new(Els::default()),
            fly: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
        });
        toolbar.style_root();
        let _ = host.append_child(&root);
        toolbar
    }

    pub fn dispose(&self) {
        self.close_flyout();
        self.listeners.borrow_mut().clear();
        self.root.remove();
    }

    // ── helpers ──

    fn tr(&self) -> Tr {
        self.ui.upgrade().map(|u| u.tr()).unwrap_or_default()
    }

    fn t(&self, m: text::M) -> String {
        tx(&self.tr(), m)
    }

    fn retire(&self, listeners: Vec<Listener>) {
        match self.ui.upgrade() {
            Some(ui) => ui.retire(listeners),
            None => drop(listeners),
        }
    }

    /// Run `f` with the controller. No `RefCell` of this widget may be borrowed by the
    /// caller: the controller answers by re-entering [`apply`](Self::apply).
    fn with_ctl(&self, f: impl FnOnce(&vela_core::render::drawings::UserDrawingController)) {
        DrawingsUi::with_ctl(&self.ui, f);
    }

    /// A listener on `el` that runs `f` on this toolbar, and is dropped with the next
    /// rebuild.
    fn listen(&self, el: &Element, ty: &'static str, f: impl Fn(&Toolbar, &JsValue) + 'static) {
        let me = self.me.clone();
        let listener = on(el, ty, move |ev: JsValue| {
            if let Some(tb) = me.upgrade() {
                f(&tb, &ev);
            }
        });
        self.listeners.borrow_mut().push(listener);
    }

    fn style_root(&self) {
        let (collapsed, visible) = {
            let st = self.st.borrow();
            (st.collapsed, st.visible)
        };
        let width = if collapsed {
            TOOLBAR_COLLAPSED_WIDTH
        } else {
            TOOLBAR_WIDTH
        };
        attr(
            &self.root,
            "data-collapsed",
            if collapsed { "1" } else { "" },
        );
        set_styles(
            &self.root,
            &[
                ("position", "absolute"),
                ("left", "0"),
                ("top", "0"),
                ("height", "100%"),
                ("width", &format!("{width}px")),
                ("z-index", "21"),
                ("display", if visible { "flex" } else { "none" }),
                ("flex-direction", "column"),
                ("gap", "4px"),
                ("padding", "6px 0"),
                ("pointer-events", "auto"),
                ("overflow-y", "auto"),
                ("overflow-x", "hidden"),
                ("cursor", "default"),
                ("user-select", "none"),
            ],
        );
    }

    // ── a snapshot arrives ──

    pub fn apply(&self, s: &ToolbarUiState) {
        let (rebuild, active_changed, visible_changed) = {
            let mut st = self.st.borrow_mut();
            let first = !st.built;
            let changed = st.raw != s.definition;
            if changed {
                st.raw = s.definition.clone();
                st.def = serde_json::from_value(s.definition.clone()).unwrap_or_default();
                let firsts: Vec<(String, String)> = st
                    .def
                    .groups
                    .iter()
                    .filter_map(|g| g.tools.first().map(|t| (g.id.clone(), t.type_key.clone())))
                    .collect();
                for (group, tool) in firsts {
                    st.last_used.entry(group).or_insert(tool);
                }
            }
            let active_changed = st.active != s.active_tool;
            let visible_changed = st.visible != s.visible;
            st.active = s.active_tool.clone();
            st.favorites = s.favorites.clone();
            st.shortcuts = s.shortcuts.clone();
            st.magnet = s.magnet;
            if s.magnet != SnapMode::Off {
                st.last_magnet_on = s.magnet;
            }
            st.stay = s.stay_mode;
            st.measure = s.measure_active;
            st.eraser = s.eraser_active;
            st.visible = s.visible;
            // The group of the armed tool remembers it: its icon arms it next time.
            if let Some(active) = st.active.clone() {
                let group = st
                    .def
                    .groups
                    .iter()
                    .find(|g| g.tools.iter().any(|t| t.type_key == active))
                    .map(|g| g.id.clone());
                if let Some(group) = group {
                    st.last_used.insert(group, active);
                }
            }
            st.built = true;
            (first || changed, active_changed, visible_changed)
        };
        if rebuild || visible_changed || active_changed {
            self.close_flyout(); // arming a tool, a new definition or hiding ends an open menu
        }
        if visible_changed {
            self.style_root();
        }
        if rebuild {
            self.rebuild();
        } else {
            self.paint_all();
            // Starring is a side action: an open flyout updates its stars in place.
            self.paint_stars();
        }
    }

    /// The chart's language changed: every label is read again.
    pub fn relocalize(&self) {
        if !self.st.borrow().built {
            return;
        }
        self.close_flyout();
        self.rebuild();
    }

    // ── building ──

    fn rebuild(&self) {
        let old = std::mem::take(&mut *self.listeners.borrow_mut());
        self.retire(old);
        while let Some(child) = self.root.first_child() {
            child.unchecked_ref::<Element>().remove();
        }
        let groups = self.st.borrow().def.groups.clone();
        let mut els = Els::default();

        let cursor = self.make_button(icon("cursor"), &self.t(text::CURSOR), |tb, _| {
            tb.on_cursor()
        });
        let _ = self.root.append_child(&cursor);
        els.cursor = Some(cursor);
        if !groups.is_empty() {
            let _ = self.root.append_child(&self.divider());
        }
        for g in &groups {
            let (cell, icon_btn, arrow) = self.make_group_cell(g);
            let _ = self.root.append_child(&cell);
            els.groups.push(GroupEls {
                id: g.id.clone(),
                cell,
                icon: icon_btn,
                arrow,
                shown: String::new(),
            });
        }
        // Measure ruler, eraser and magnet are renderer-local modes, not persistent tools.
        let _ = self.root.append_child(&self.divider());
        let measure = self.make_button(icon("ruler"), &self.t(text::MEASURE), |tb, _| {
            tb.with_ctl(|c| c.toolbar_toggle_measure());
        });
        let _ = self.root.append_child(&measure);
        els.measure = Some(measure);
        let eraser = self.make_button(icon("eraser"), &self.t(text::ERASER), |tb, _| {
            tb.with_ctl(|c| c.toolbar_toggle_eraser());
        });
        let _ = self.root.append_child(&eraser);
        els.eraser = Some(eraser);
        let (cell, magnet_icon, magnet_arrow) = self.make_magnet_cell();
        let _ = self.root.append_child(&cell);
        els.magnet_cell = Some(cell);
        els.magnet_icon = Some(magnet_icon);
        els.magnet_arrow = magnet_arrow;
        let stay = self.make_button(icon("pen-lock"), &self.t(text::STAY), |tb, _| tb.on_stay());
        let _ = self.root.append_child(&stay);
        els.stay = Some(stay);
        // Pinned to the bottom; the only child a collapsed strip shows.
        let collapse = self.make_button(icon("chevrons-left"), &self.t(text::COLLAPSE), |tb, _| {
            tb.on_collapse();
        });
        collapse.set_class_name("vela-dtb-btn vela-dtb-collapse");
        let _ = self.root.append_child(&collapse);
        els.collapse = Some(collapse);

        *self.els.borrow_mut() = els;
        self.paint_all();
    }

    fn divider(&self) -> Element {
        let d = make(&self.doc, "div", "");
        set_styles(
            &d,
            &[
                ("width", "24px"),
                ("height", "1px"),
                ("margin", "2px auto"),
                ("flex", "none"),
                ("background", "var(--dtb-border)"),
            ],
        );
        attr(&d, "role", "separator");
        d
    }

    /// A single-icon button.
    fn make_button(
        &self,
        svg: &str,
        label: &str,
        on_click: impl Fn(&Toolbar, &JsValue) + 'static,
    ) -> Element {
        let btn = make(&self.doc, "button", "vela-dtb-btn");
        attr(&btn, "type", "button");
        attr(&btn, "aria-label", label);
        attr(&btn, "title", label);
        btn.set_inner_html(&hit_html(svg, 18));
        self.listen(&btn, "click", on_click);
        btn
    }

    /// A cell: a full-width icon button (arms or toggles the primary action) plus, when
    /// `multi`, a chevron button pinned to the right that opens the flyout.
    fn make_cell(
        &self,
        label: &str,
        svg: &str,
        on_icon: impl Fn(&Toolbar) + 'static,
        on_arrow: Option<impl Fn(&Toolbar, bool) + 'static>,
    ) -> (Element, Element, Option<Element>) {
        let cell = make(&self.doc, "div", "vela-dtb-cell");
        let icon_btn = make(&self.doc, "button", "vela-dtb-icon");
        attr(&icon_btn, "type", "button");
        attr(&icon_btn, "aria-label", label);
        attr(&icon_btn, "title", label);
        if !svg.is_empty() {
            icon_btn.set_inner_html(&hit_html(svg, 18));
        }
        self.listen(&icon_btn, "click", move |tb, _| on_icon(tb));
        let _ = cell.append_child(&icon_btn);
        let mut arrow = None;
        if let Some(on_arrow) = on_arrow {
            let a = make(&self.doc, "button", "vela-dtb-arrow");
            attr(&a, "type", "button");
            attr(&a, "aria-label", &more_label(&self.tr(), label));
            attr(&a, "aria-haspopup", "menu");
            attr(&a, "aria-expanded", "false");
            a.set_inner_html(&arrow_hit_html(icon("chevron-right")));
            self.listen(&a, "click", move |tb, ev| {
                stop_propagation(ev);
                // `detail` is 0 for a keyboard-synthesized click.
                let keyboard = get_f64(ev, "detail").is_some_and(|d| d == 0.0);
                on_arrow(tb, keyboard);
            });
            let _ = cell.append_child(&a);
            arrow = Some(a);
        }
        (cell, icon_btn, arrow)
    }

    fn make_group_cell(&self, g: &ToolGroup) -> (Element, Element, Option<Element>) {
        let multi = g.tools.len() > 1;
        let id = g.id.clone();
        let id_arrow = g.id.clone();
        let arrow = multi.then_some(move |tb: &Toolbar, keyboard: bool| {
            tb.on_group_arrow(&id_arrow, keyboard);
        });
        self.make_cell(&g.label, "", move |tb| tb.on_group_icon(&id), arrow)
    }

    fn make_magnet_cell(&self) -> (Element, Element, Option<Element>) {
        self.make_cell(
            &self.t(text::MAGNET_SNAP),
            icon("magnet"),
            |tb| tb.toggle_magnet(),
            Some(|tb: &Toolbar, keyboard: bool| tb.on_magnet_arrow(keyboard)),
        )
    }

    // ── actions ──

    /// The cursor returns to select / idle: an active measure or eraser mode exits
    /// through its own toggle first (a null arm alone cannot).
    fn on_cursor(&self) {
        let (measure, eraser) = {
            let st = self.st.borrow();
            (st.measure, st.eraser)
        };
        self.with_ctl(|c| {
            if measure {
                c.toolbar_toggle_measure();
            }
            if eraser {
                c.toolbar_toggle_eraser();
            }
            c.toolbar_arm(None);
        });
    }

    /// The icon arms the group's last-used tool (it does not open the flyout).
    fn on_group_icon(&self, group: &str) {
        let tool = {
            let st = self.st.borrow();
            st.last_used.get(group).cloned().or_else(|| {
                st.def
                    .groups
                    .iter()
                    .find(|g| g.id == group)
                    .and_then(|g| g.tools.first())
                    .map(|t| t.type_key.clone())
            })
        };
        if let Some(tool) = tool {
            self.with_ctl(|c| c.toolbar_arm(Some(&tool)));
        }
    }

    /// The chevron toggles the group's flyout (same group closes; another switches).
    fn on_group_arrow(&self, group: &str, keyboard: bool) {
        let was_open = self.fly.borrow().as_ref().is_some_and(|f| f.owner == group);
        self.close_flyout();
        if was_open {
            return;
        }
        let g = self
            .st
            .borrow()
            .def
            .groups
            .iter()
            .find(|g| g.id == group)
            .cloned();
        if let Some(g) = g {
            self.open_group_flyout(&g, keyboard);
        }
    }

    fn toggle_magnet(&self) {
        let next = {
            let st = self.st.borrow();
            if st.magnet == SnapMode::Off {
                st.last_magnet_on
            } else {
                SnapMode::Off
            }
        };
        self.apply_magnet(next);
    }

    fn on_magnet_arrow(&self, keyboard: bool) {
        let was_open = self
            .fly
            .borrow()
            .as_ref()
            .is_some_and(|f| f.owner == MAGNET_ID);
        self.close_flyout();
        if !was_open {
            self.open_magnet_flyout(keyboard);
        }
    }

    /// Set the magnet mode, tell the controller and repaint (the icon toggle and the menu).
    fn apply_magnet(&self, mode: SnapMode) {
        {
            let mut st = self.st.borrow_mut();
            st.magnet = mode;
            if mode != SnapMode::Off {
                st.last_magnet_on = mode;
            }
        }
        self.with_ctl(|c| c.toolbar_set_snap_mode(mode));
        self.paint_all();
    }

    fn on_stay(&self) {
        let on = {
            let mut st = self.st.borrow_mut();
            st.stay = !st.stay;
            st.stay
        };
        self.with_ctl(|c| c.toolbar_stay_mode(on));
        self.paint_all();
    }

    /// Collapse to the slim expand strip, or restore the bar; the gutter follows through
    /// the controller.
    fn on_collapse(&self) {
        let collapsed = {
            let mut st = self.st.borrow_mut();
            st.collapsed = !st.collapsed;
            st.collapsed
        };
        self.close_flyout();
        self.style_root();
        self.paint_all();
        self.with_ctl(|c| c.toolbar_collapsed(collapsed));
    }

    // ── painting ──

    fn paint_all(&self) {
        let tr = self.tr();
        let st = self.st.borrow();
        let mut els = self.els.borrow_mut();
        let active_group = st.active.as_ref().and_then(|a| {
            st.def
                .groups
                .iter()
                .find(|g| g.tools.iter().any(|t| &t.type_key == a))
                .map(|g| g.id.clone())
        });
        for ge in &mut els.groups {
            let Some(g) = st.def.groups.iter().find(|g| g.id == ge.id) else {
                continue;
            };
            let on = active_group.as_deref() == Some(g.id.as_str());
            set_active(&ge.cell, on);
            attr(&ge.icon, "aria-pressed", bool_str(on));
            // The icon (and the tip) name the tool it arms: the group's last used.
            let wanted = st
                .last_used
                .get(&g.id)
                .cloned()
                .or_else(|| g.tools.first().map(|t| t.type_key.clone()))
                .unwrap_or_default();
            if ge.shown != wanted {
                let tool = g
                    .tools
                    .iter()
                    .find(|t| t.type_key == wanted)
                    .or_else(|| g.tools.first());
                ge.icon
                    .set_inner_html(&hit_html(tool.map_or("", |t| t.icon.as_str()), 18));
                let label = tool.map_or(g.label.as_str(), |t| t.label.as_str());
                attr(&ge.icon, "aria-label", label);
                attr(&ge.icon, "title", label);
                if let Some(arrow) = &ge.arrow {
                    attr(arrow, "aria-label", &more_label(&tr, label));
                }
                ge.shown = wanted;
            }
        }
        if let Some(cursor) = &els.cursor {
            let on = st.active.is_none() && !st.measure && !st.eraser;
            set_active(cursor, on);
            attr(cursor, "aria-pressed", bool_str(on));
        }
        if let Some(b) = &els.measure {
            set_active(b, st.measure);
            attr(b, "aria-pressed", bool_str(st.measure));
        }
        if let Some(b) = &els.eraser {
            set_active(b, st.eraser);
            attr(b, "aria-pressed", bool_str(st.eraser));
        }
        if let Some(cell) = &els.magnet_cell {
            let on = st.magnet != SnapMode::Off;
            let tip = match st.magnet {
                SnapMode::Off => self.t(text::MAGNET_OFF),
                SnapMode::Weak => self.t(text::MAGNET_WEAK),
                SnapMode::Strong => self.t(text::MAGNET_STRONG),
            };
            set_active(cell, on);
            if let Some(btn) = &els.magnet_icon {
                set_style(btn, "opacity", if on { "1" } else { "0.5" });
                attr(btn, "aria-label", &tip);
                attr(btn, "title", &tip);
                attr(btn, "aria-pressed", bool_str(on));
            }
        }
        if let Some(b) = &els.stay {
            set_active(b, st.stay);
            set_style(b, "opacity", if st.stay { "1" } else { "0.5" });
            attr(b, "aria-pressed", bool_str(st.stay));
        }
        if let Some(b) = &els.collapse {
            let (svg, label) = if st.collapsed {
                (icon("chevrons-right"), self.t(text::EXPAND))
            } else {
                (icon("chevrons-left"), self.t(text::COLLAPSE))
            };
            b.set_inner_html(&hit_html(svg, 12));
            attr(b, "aria-label", &label);
            attr(b, "title", &label);
            attr(b, "aria-expanded", bool_str(!st.collapsed));
        }
    }

    /// Repaint the open flyout's stars for the current favorites.
    fn paint_stars(&self) {
        let fly = self.fly.borrow();
        let Some(fly) = fly.as_ref() else { return };
        let st = self.st.borrow();
        for (tool, star) in &fly.stars {
            paint_star(&self.tr(), star, st.favorites.contains(tool));
        }
    }

    // ── flyouts ──

    /// Create the positioned flyout panel anchored to `cell`, register it as open and start
    /// the outside-press dismissal. The caller fills it with items.
    fn begin_flyout(&self, owner: &str, cell: &Element, opener: &Element) -> Element {
        let fly = make(&self.doc, "div", "vela-dtb-flyout");
        attr(&fly, "role", "menu");
        let r = cell.get_bounding_client_rect();
        let root_r = self.root.get_bounding_client_rect();
        let host_r = self.host.get_bounding_client_rect();
        let top = r.top() - host_r.top();
        set_styles(
            &fly,
            &[
                (
                    "left",
                    &format!("{}px", root_r.right() - host_r.left() + FLYOUT_GAP),
                ),
                ("top", &format!("{top}px")),
                // Cap the height to the room left so a long tool list scrolls.
                (
                    "max-height",
                    &format!("{}px", (host_r.height() - top - 8.0).max(120.0)),
                ),
            ],
        );
        let _ = self.host.append_child(&fly);
        cell.set_class_name("vela-dtb-cell vela-open");
        attr(opener, "aria-expanded", "true");
        // The press that opened it has already fired: dismiss on the next OUTSIDE press.
        let me = self.me.clone();
        let root = self.root.clone();
        let fly_el = fly.clone();
        let outside = Listener::new(
            self.doc.as_ref(),
            "pointerdown",
            false,
            true,
            move |ev: JsValue| {
                let target = get_prop(&ev, "target");
                // Presses inside the bar (cells, chevrons) are handled by their own click
                // handlers: closing here would pre-empt a chevron's toggle.
                if vela_contains(&fly_el, &target) || vela_contains(&root, &target) {
                    return;
                }
                if let Some(tb) = me.upgrade() {
                    tb.close_flyout();
                }
            },
        );
        *self.fly.borrow_mut() = Some(Flyout {
            owner: owner.to_string(),
            el: fly.clone(),
            cell: cell.clone(),
            opener: opener.clone(),
            items: Vec::new(),
            stars: Vec::new(),
            listeners: Vec::new(),
            outside,
        });
        fly
    }

    fn close_flyout(&self) {
        let fly = self.fly.borrow_mut().take();
        let Some(f) = fly else { return };
        f.el.remove();
        f.cell.set_class_name("vela-dtb-cell");
        attr(&f.opener, "aria-expanded", "false");
        let mut listeners = f.listeners;
        listeners.push(f.outside);
        self.retire(listeners);
    }

    /// Wire keyboard navigation and store the finished flyout's items and listeners.
    fn finish_flyout(
        &self,
        fly: &Element,
        items: Vec<Element>,
        stars: Vec<(String, Element)>,
        mut listeners: Vec<Listener>,
        focus_first: bool,
    ) {
        let me = self.me.clone();
        listeners.push(on(fly, "keydown", move |ev: JsValue| {
            if let Some(tb) = me.upgrade() {
                tb.on_flyout_key(&ev);
            }
        }));
        if focus_first && let Some(first) = items.first() {
            vela_focus(first);
        }
        if let Some(f) = self.fly.borrow_mut().as_mut() {
            f.items = items;
            f.stars = stars;
            f.listeners = listeners;
        }
    }

    fn on_flyout_key(&self, ev: &JsValue) {
        let key = get_str(ev, "key");
        let target = get_prop(ev, "target");
        match key.as_str() {
            "Escape" => {
                prevent_default(ev);
                let opener = self.fly.borrow().as_ref().map(|f| f.opener.clone());
                self.close_flyout();
                if let Some(opener) = opener {
                    vela_focus(&opener);
                }
            }
            "ArrowDown" | "ArrowUp" => {
                prevent_default(ev);
                let items = self
                    .fly
                    .borrow()
                    .as_ref()
                    .map(|f| f.items.clone())
                    .unwrap_or_default();
                if items.is_empty() {
                    return;
                }
                let at = items.iter().position(|i| vela_contains(i, &target));
                let next = match (key.as_str(), at) {
                    ("ArrowDown", Some(i)) => (i + 1) % items.len(),
                    ("ArrowDown", None) => 0,
                    (_, Some(i)) => (i + items.len() - 1) % items.len(),
                    (_, None) => items.len() - 1,
                };
                vela_focus(&items[next]);
            }
            _ => {}
        }
    }

    fn open_group_flyout(&self, group: &ToolGroup, keyboard: bool) {
        let (cell, opener) = {
            let els = self.els.borrow();
            let Some(ge) = els.groups.iter().find(|g| g.id == group.id) else {
                return;
            };
            (
                ge.cell.clone(),
                ge.arrow.clone().unwrap_or_else(|| ge.icon.clone()),
            )
        };
        let fly = self.begin_flyout(&group.id, &cell, &opener);
        let (active, shortcuts, favorites) = {
            let st = self.st.borrow();
            (
                st.active.clone(),
                st.shortcuts.clone(),
                st.favorites.clone(),
            )
        };
        let fallback;
        let sections: &[ToolSection] = match &group.sections {
            Some(s) => s,
            None => {
                fallback = [ToolSection {
                    label: String::new(),
                    tools: group.tools.clone(),
                }];
                &fallback
            }
        };
        let badge = sections
            .iter()
            .any(|s| s.tools.iter().any(|t| !t.icon.is_empty()));
        let mut items = Vec::new();
        let mut stars = Vec::new();
        let mut listeners = Vec::new();
        for (si, section) in sections.iter().enumerate() {
            if !section.label.is_empty() {
                if si > 0 {
                    let sep = make(&self.doc, "div", "vela-dtb-sep");
                    let _ = fly.append_child(&sep);
                }
                let header = make(&self.doc, "div", "vela-dtb-header");
                header.set_text_content(Some(&section.label));
                let _ = fly.append_child(&header);
            }
            for tool in &section.tools {
                let type_key = tool.type_key.clone();
                let favorite = favorites.contains(&type_key);
                let (item, star) = self.make_flyout_item(
                    &mut listeners,
                    FlyoutItem {
                        icon: &tool.icon,
                        badge,
                        label: &tool.label,
                        selected: active.as_deref() == Some(type_key.as_str()),
                        shortcut: shortcuts.get(&type_key).map(String::as_str),
                        favorite: Some((&type_key, favorite)),
                    },
                    {
                        let type_key = type_key.clone();
                        move |tb| {
                            // The core closes the flyout when it arms the tool; arming the
                            // armed tool changes nothing, so close it here as well.
                            tb.close_flyout();
                            tb.with_ctl(|c| c.toolbar_arm(Some(&type_key)));
                        }
                    },
                );
                let _ = fly.append_child(&item);
                items.push(item);
                if let Some(star) = star {
                    stars.push((type_key, star));
                }
            }
        }
        self.finish_flyout(&fly, items, stars, listeners, keyboard);
    }

    fn open_magnet_flyout(&self, keyboard: bool) {
        let (cell, opener) = {
            let els = self.els.borrow();
            let (Some(cell), Some(icon_btn)) = (&els.magnet_cell, &els.magnet_icon) else {
                return;
            };
            (
                cell.clone(),
                els.magnet_arrow.clone().unwrap_or_else(|| icon_btn.clone()),
            )
        };
        let fly = self.begin_flyout(MAGNET_ID, &cell, &opener);
        let header = make(&self.doc, "div", "vela-dtb-header");
        header.set_text_content(Some(&self.t(text::MAGNET)));
        let _ = fly.append_child(&header);
        let current = self.st.borrow().magnet;
        let mut items = Vec::new();
        let mut listeners = Vec::new();
        for (mode, label) in [
            (SnapMode::Off, self.t(text::OFF)),
            (SnapMode::Weak, self.t(text::WEAK)),
            (SnapMode::Strong, self.t(text::STRONG)),
        ] {
            let (item, _) = self.make_flyout_item(
                &mut listeners,
                FlyoutItem {
                    icon: "",
                    badge: false,
                    label: &label,
                    selected: mode == current,
                    shortcut: None,
                    favorite: None,
                },
                move |tb| {
                    tb.apply_magnet(mode);
                    tb.close_flyout();
                },
            );
            let _ = fly.append_child(&item);
            items.push(item);
        }
        self.finish_flyout(&fly, items, Vec::new(), listeners, keyboard);
    }

    /// A flyout row, left to right: an optional icon badge, the label, a check when it is
    /// the selected entry, an optional shortcut hint and, at the far right, the favorite
    /// star (tool rows). Returns the row and its star.
    fn make_flyout_item(
        &self,
        listeners: &mut Vec<Listener>,
        opts: FlyoutItem<'_>,
        on_select: impl Fn(&Toolbar) + 'static,
    ) -> (Element, Option<Element>) {
        let item = make(&self.doc, "button", "vela-dtb-item");
        attr(&item, "type", "button");
        attr(&item, "role", "menuitem");
        attr(&item, "aria-label", opts.label);
        if opts.badge || !opts.icon.is_empty() {
            attr(&item, "data-badged", "1");
            let badge = make(&self.doc, "span", "vela-dtb-badge");
            if !opts.icon.is_empty() {
                badge.set_inner_html(opts.icon);
            }
            let _ = item.append_child(&badge);
        }
        if opts.selected {
            attr(&item, "data-selected", "1");
        }
        let label = make(&self.doc, "span", "vela-dtb-label");
        label.set_text_content(Some(opts.label));
        let _ = item.append_child(&label);
        let check = make(&self.doc, "span", "vela-dtb-check");
        if opts.selected {
            check.set_inner_html(&sized_icon(icon("check")));
        }
        let _ = item.append_child(&check);
        if let Some(shortcut) = opts.shortcut {
            let hint = make(&self.doc, "span", "vela-dtb-hint");
            hint.set_text_content(Some(shortcut));
            let _ = item.append_child(&hint);
        }
        let mut star_el = None;
        if let Some((type_key, favorite)) = opts.favorite {
            let star = make(&self.doc, "span", "vela-dtb-star");
            attr(&star, "role", "button");
            attr(&star, "tabindex", "0");
            paint_star(&self.tr(), &star, favorite);
            // Swallow the whole pointer sequence: the row arms its tool on click, and a
            // press that starts on the star must reach neither it nor the flyout's
            // outside-dismiss listener.
            for ev in ["pointerdown", "pointerup", "mousedown", "mouseup"] {
                listeners.push(on(&star, ev, |e: JsValue| stop_propagation(&e)));
            }
            let toggle = {
                let me = self.me.clone();
                let type_key = type_key.to_string();
                move || {
                    let Some(tb) = me.upgrade() else { return };
                    let on = !tb.st.borrow().favorites.contains(&type_key);
                    tb.with_ctl(|c| c.toolbar_favorite(&type_key, on));
                }
            };
            let toggle = Rc::new(toggle);
            {
                let toggle = toggle.clone();
                listeners.push(on(&star, "click", move |e: JsValue| {
                    stop_propagation(&e);
                    prevent_default(&e);
                    toggle();
                }));
            }
            listeners.push(on(&star, "keydown", move |e: JsValue| {
                let key = get_str(&e, "key");
                if key == "Enter" || key == " " {
                    stop_propagation(&e);
                    prevent_default(&e);
                    toggle();
                }
            }));
            let _ = item.append_child(&star);
            star_el = Some(star);
        }
        let me = self.me.clone();
        listeners.push(on(&item, "click", move |e: JsValue| {
            stop_propagation(&e);
            if let Some(tb) = me.upgrade() {
                on_select(&tb);
            }
        }));
        (item, star_el)
    }
}

/// What one flyout row shows.
struct FlyoutItem<'a> {
    icon: &'a str,
    /// Reserve the badge column on an icon-less row of a flyout whose other rows carry
    /// icons, so labels align.
    badge: bool,
    label: &'a str,
    selected: bool,
    /// A pre-formatted shortcut hint.
    shortcut: Option<&'a str>,
    /// The tool type and whether it is a favorite (tool rows only).
    favorite: Option<(&'a str, bool)>,
}

fn bool_str(on: bool) -> &'static str {
    if on { "true" } else { "false" }
}

/// `data-active` is what the stylesheet paints: no inline background, so a hover never
/// sticks.
fn set_active(el: &Element, on: bool) {
    attr(el, "data-active", if on { "1" } else { "" });
}

/// One star for the current favorite state (filled and gold, or an outline).
fn paint_star(tr: &Tr, el: &Element, on: bool) {
    el.set_class_name(if on {
        "vela-dtb-star vela-fav"
    } else {
        "vela-dtb-star"
    });
    attr(
        el,
        "aria-label",
        &tx(
            tr,
            if on {
                text::REMOVE_FAVORITE
            } else {
                text::ADD_FAVORITE
            },
        ),
    );
    attr(el, "aria-pressed", bool_str(on));
    el.set_inner_html(&sized_icon(icon(if on { "star-filled" } else { "star" })));
}
