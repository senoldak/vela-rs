//! The sidebar (`sidebar.ts`): the shell's right-edge toolbar. It hosts the side-panel toggles
//! (the dock drives it as its chrome) and the actions plugins register with target `sidebar`,
//! laid out by the `sidebar` composition. Desktop only (the mobile layout hides it and keeps
//! the panels' menu rows).
//!
//! State and layout are `vela_core::widget::sidebar::SidebarState`; this paints it as
//! `nav.vela-sidebar` in [`Region::Edge`], two sections (`top`, `bottom`) of groups split by
//! separators, one `button.vela-sidebar-btn` per entry with an accessible name (`aria-label`),
//! a native tooltip, `aria-pressed` and an optional badge.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::widget::contributions::{
    Contributions, SidebarBadge, SidebarControl, WidgetContext,
};
use vela_core::widget::shell_defaults::SidebarOption;
use vela_core::widget::sidebar::{
    SidebarButtonEntry, SidebarHandle, SidebarState, sidebar_actions, sidebar_label,
};
use vela_core::widget::sidebar_composition::{SidebarComposition, SidebarKind};
use web_sys::Element;

use crate::dom::attr;
use crate::workspace_ui::{Mount, Region};

use super::dock::{CtxFactory, Dock};
use super::kit::{Bag, Cx, clear, set_data, set_title};

pub struct Sidebar {
    cx: Cx,
    me: Weak<Sidebar>,
    pub state: Rc<SidebarState>,
    el: Element,
    top: Element,
    bottom: Element,
    bag: Rc<Bag>,
    buttons: RefCell<Vec<(String, Element)>>,
    dock: Weak<Dock>,
    contributions: Option<Contributions>,
    context: RefCell<Option<CtxFactory>>,
    mount: RefCell<Option<Mount>>,
}

/// The composition a `sidebar` option asks for (`true`: the default layout).
pub fn composition_of(option: &SidebarOption) -> Option<SidebarComposition> {
    match option {
        SidebarOption::Enabled(true) => Some(SidebarComposition::default()),
        SidebarOption::Enabled(false) => None,
        SidebarOption::Composition(c) => Some(c.clone()),
    }
}

impl Sidebar {
    pub fn new(
        cx: &Cx,
        composition: SidebarComposition,
        dock: &Rc<Dock>,
        contributions: Option<Contributions>,
    ) -> Rc<Sidebar> {
        let el = cx.el("nav", "vela-sidebar");
        let top = cx.el("div", "vela-sidebar-section");
        attr(&top, "data-section", "top");
        let bottom = cx.el("div", "vela-sidebar-section");
        attr(&bottom, "data-section", "bottom");
        let _ = el.append_child(&top);
        let _ = el.append_child(&bottom);
        let mount = cx.root.mount(Region::Edge, &el);
        let sb = Rc::new_cyclic(|me| Sidebar {
            cx: cx.clone(),
            me: me.clone(),
            state: Rc::new(SidebarState::new(composition)),
            el,
            top,
            bottom,
            bag: Bag::new(),
            buttons: RefCell::default(),
            dock: Rc::downgrade(dock),
            contributions,
            context: RefCell::new(None),
            mount: RefCell::new(Some(mount)),
        });
        sb.relocalize();
        sb
    }

    pub fn element(&self) -> &Element {
        &self.el
    }

    pub fn set_context_factory(&self, f: CtxFactory) {
        *self.context.borrow_mut() = Some(f);
    }

    /// The panel group from the dock (dock order).
    pub fn set_panel_buttons(&self, buttons: &[vela_core::widget::contributions::SidePanelButton]) {
        self.state.set_panel_buttons(buttons);
        self.render();
    }

    pub fn set_panel_active(&self, id: &str, open: bool) {
        self.state.set_panel_active(id, open);
        let found = self
            .buttons
            .borrow()
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, b)| b.clone());
        if let Some(b) = found {
            paint_active(&b, open);
        }
    }

    pub fn set_badge(&self, id: &str, badge: Option<&SidebarBadge>) {
        if self.state.set_badge(id, badge) {
            self.render();
        }
    }

    /// The shell finished building: the actions (which read the context) show from now on.
    pub fn activate(&self) {
        self.state.activate();
        self.render();
    }

    pub fn relocalize(&self) {
        let tr = self.cx.tr();
        let label = vela_core::i18n::text_of(tr.as_deref(), &sidebar_label(), &[]);
        attr(&self.el, "aria-label", &label);
        self.render();
    }

    /// `ctx.sidebar` over this sidebar.
    pub fn control(self: &Rc<Self>) -> Rc<dyn SidebarControl> {
        let a = self.me.clone();
        let b = self.me.clone();
        Rc::new(SidebarHandle::new(
            move || a.upgrade().map(|s| s.state.clone()),
            move || {
                if let Some(s) = b.upgrade() {
                    s.render();
                }
            },
        ))
    }

    /// Whether an entry shows right now (what the mobile menu filters on).
    pub fn is_visible(&self, id: &str) -> bool {
        self.state.is_visible(id)
    }

    fn ctx(&self) -> Option<Rc<dyn WidgetContext>> {
        self.context.borrow().as_ref().map(|f| f())
    }

    /// Redraw from the dock's panels and the registered sidebar actions.
    pub fn render(&self) {
        self.bag.retire(&self.cx);
        self.buttons.borrow_mut().clear();
        let ctx = self.ctx();
        let tr = self.cx.tr();
        let contributions = self
            .contributions
            .clone()
            .unwrap_or_else(|| Contributions::new(self.cx.root.platform().clone()));
        let layout = self
            .state
            .layout(&contributions, ctx.as_deref(), tr.as_deref());
        clear(&self.top);
        clear(&self.bottom);
        for g in self.groups(&layout.top) {
            let _ = self.top.append_child(&g);
        }
        for g in self.groups(&layout.bottom) {
            let _ = self.bottom.append_child(&g);
        }
    }

    fn groups(&self, groups: &[Vec<SidebarButtonEntry>]) -> Vec<Element> {
        let mut out = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            if i > 0 {
                let sep = self.cx.el("div", "vela-sidebar-sep");
                attr(&sep, "role", "separator");
                out.push(sep);
            }
            let group = self.cx.el("div", "vela-sidebar-group");
            for e in g {
                let _ = group.append_child(&self.button(e));
            }
            out.push(group);
        }
        out
    }

    fn button(&self, e: &SidebarButtonEntry) -> Element {
        let b = self.cx.el("button", "vela-sidebar-btn");
        attr(&b, "type", "button");
        set_data(&b, "id", Some(&e.id));
        set_data(
            &b,
            "kind",
            Some(if e.kind == SidebarKind::Action {
                "action"
            } else {
                "panel"
            }),
        );
        if e.kind == SidebarKind::Action {
            set_data(&b, "action-id", Some(&e.id));
        } else {
            set_data(&b, "panel-id", Some(&e.id));
        }
        attr(&b, "aria-label", &e.title);
        set_title(&b, &e.title);
        let _ = b.append_child(&self.cx.icon(&e.icon));
        if let Some(text) = self.state.badge(&e.id) {
            let span = self.cx.text("span", "vela-sidebar-badge", &text);
            let _ = b.append_child(&span);
        }
        paint_active(&b, e.active);
        let me = self.me.clone();
        let id = e.id.clone();
        let kind = e.kind;
        self.bag.on(&b, "click", move |_| {
            let Some(s) = me.upgrade() else { return };
            match kind {
                SidebarKind::Panel => {
                    if let Some(d) = s.dock.upgrade() {
                        d.toggle(&id, None);
                    }
                }
                SidebarKind::Action => s.run_action(&id),
            }
        });
        self.buttons.borrow_mut().push((e.id.clone(), b.clone()));
        b
    }

    fn run_action(&self, id: &str) {
        let (Some(contrib), Some(ctx)) = (self.contributions.clone(), self.ctx()) else {
            return;
        };
        let action = sidebar_actions(&contrib, &*ctx)
            .into_iter()
            .find(|a| a.id == id);
        if let Some(a) = action {
            // A fresh context for the run (the original builds one per call).
            let fresh = self.ctx().unwrap_or(ctx);
            (a.run)(&*fresh);
            self.render();
        }
    }

    pub fn destroy(&self) {
        self.bag.clear();
        *self.mount.borrow_mut() = None;
        self.el.remove();
    }
}

fn paint_active(b: &Element, on: bool) {
    set_data(b, "active", Some(if on { "1" } else { "" }));
    attr(b, "aria-pressed", if on { "true" } else { "false" });
}
