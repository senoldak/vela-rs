//! The ONE [`WidgetContext`] of the widget: what contributed actions, panels, attachments and
//! legend actions are handed, built by the root from the workspace and the slots the shell
//! parts fill (the dock, the sidebar, the topbar's item visibility, the symbol search).
//!
//! The root owns it (`WidgetRoot::context()`), `shell_top` and `shell_side` share it. A part
//! that is not mounted leaves its slot on the stand-in: an empty dock, a sidebar that is not
//! there, a topbar that reports every item visible and remembers what a plugin hid.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use vela_core::i18n::Translator;
use vela_core::runtime::Unsubscribe;
use vela_core::vela::Vela;
use vela_core::widget::contributions::{
    ContextMenuPointer, DockEdge, DockPosition, DockStripOptions, DockedPanelInfo, DockedStrip,
    ElementHandle, ExternalIndicatorEntry, PanelDockView, SidebarBadge, SidebarControl, ToastKind,
    TopbarItemId, TopbarItems, WidgetContext,
};
use vela_core::workspace::{ExternalIndicatorEntry as CellEntry, WeakWorkspace, Workspace};
use web_sys::Element;

use crate::dom::{attr, make};

use super::root::{Region, WidgetRoot};

/// Topbar visibility held locally until a real topbar replaces it.
#[derive(Default)]
pub struct LocalTopbar {
    hidden: RefCell<HashSet<&'static str>>,
}

impl TopbarItems for LocalTopbar {
    fn is_visible(&self, id: TopbarItemId) -> bool {
        !self.hidden.borrow().contains(id.as_str())
    }
    fn set_visible(&self, id: TopbarItemId, visible: bool) {
        let mut h = self.hidden.borrow_mut();
        if visible {
            h.remove(id.as_str());
        } else {
            h.insert(id.as_str());
        }
    }
}

struct NoPanels;

impl PanelDockView for NoPanels {
    fn list(&self) -> Vec<DockedPanelInfo> {
        Vec::new()
    }
    fn open_id(&self) -> Option<String> {
        None
    }
    fn is_maximized(&self, _: &str) -> bool {
        false
    }
    fn on_did_change(&self, _: Box<dyn Fn()>) -> Unsubscribe {
        Box::new(|| {})
    }
}

struct NoSidebar;

impl SidebarControl for NoSidebar {
    fn enabled(&self) -> bool {
        false
    }
    fn set_badge(&self, _: &str, _: Option<SidebarBadge>) {}
    fn refresh(&self) {}
    fn is_visible(&self, _: &str) -> bool {
        false
    }
    fn set_visible(&self, _: &str, _: bool) {}
}

type PanelToggle = Rc<dyn Fn(&str, Option<bool>)>;
type SymbolSearch = Rc<dyn Fn(Option<&str>)>;

/// The parts of the context the shell components fill after they mount. Every setter
/// replaces a stand-in; contexts built afterwards see the new part.
pub struct ContextSlots {
    panels: RefCell<Rc<dyn PanelDockView>>,
    toggle_panel: RefCell<PanelToggle>,
    sidebar: RefCell<Rc<dyn SidebarControl>>,
    topbar: RefCell<Rc<dyn TopbarItems>>,
    open_symbol_search: RefCell<SymbolSearch>,
}

impl Default for ContextSlots {
    fn default() -> Self {
        Self {
            panels: RefCell::new(Rc::new(NoPanels)),
            toggle_panel: RefCell::new(Rc::new(|_, _| {})),
            sidebar: RefCell::new(Rc::new(NoSidebar)),
            topbar: RefCell::new(Rc::new(LocalTopbar::default())),
            open_symbol_search: RefCell::new(Rc::new(|_| {})),
        }
    }
}

impl ContextSlots {
    /// `ctx.panels` and `ctx.toggle_panel`: the dock.
    pub fn set_dock(&self, panels: Rc<dyn PanelDockView>, toggle: PanelToggle) {
        *self.panels.borrow_mut() = panels;
        *self.toggle_panel.borrow_mut() = toggle;
    }

    /// `ctx.sidebar`.
    pub fn set_sidebar(&self, sidebar: Rc<dyn SidebarControl>) {
        *self.sidebar.borrow_mut() = sidebar;
    }

    /// `ctx.topbar`.
    pub fn set_topbar(&self, topbar: Rc<dyn TopbarItems>) {
        *self.topbar.borrow_mut() = topbar;
    }

    /// `ctx.open_symbol_search`.
    pub fn set_symbol_search(&self, open: SymbolSearch) {
        *self.open_symbol_search.borrow_mut() = open;
    }
}

/// The context handed to contributions. A cheap handle: every read is live.
pub struct RootContext {
    root: Weak<WidgetRoot>,
    ws: WeakWorkspace,
    panels: Rc<dyn PanelDockView>,
    toggle_panel: PanelToggle,
    sidebar: Rc<dyn SidebarControl>,
    topbar: Rc<dyn TopbarItems>,
    open_symbol_search: SymbolSearch,
    pointer: Option<ContextMenuPointer>,
}

impl RootContext {
    pub(super) fn new(root: &Rc<WidgetRoot>, ws: WeakWorkspace) -> Self {
        let slots = root.slots();
        Self {
            root: Rc::downgrade(root),
            ws,
            panels: slots.panels.borrow().clone(),
            toggle_panel: slots.toggle_panel.borrow().clone(),
            sidebar: slots.sidebar.borrow().clone(),
            topbar: slots.topbar.borrow().clone(),
            open_symbol_search: slots.open_symbol_search.borrow().clone(),
            pointer: None,
        }
    }

    /// The same context with the right-click that opened a menu.
    pub fn with_pointer(&self, pointer: ContextMenuPointer) -> Self {
        Self {
            root: self.root.clone(),
            ws: self.ws.clone(),
            panels: self.panels.clone(),
            toggle_panel: self.toggle_panel.clone(),
            sidebar: self.sidebar.clone(),
            topbar: self.topbar.clone(),
            open_symbol_search: self.open_symbol_search.clone(),
            pointer: Some(pointer),
        }
    }

    fn ws(&self) -> Option<Workspace> {
        self.ws.upgrade()
    }

    /// The widget this context belongs to, while it lives.
    pub fn widget_root(&self) -> Option<Rc<WidgetRoot>> {
        self.root.upgrade()
    }

    fn root(&self) -> Option<Rc<WidgetRoot>> {
        self.root.upgrade()
    }
}

/// The element an [`ElementHandle`] carries, when it is one.
pub fn element_of(handle: &ElementHandle) -> Option<Element> {
    handle.downcast_ref::<Element>().cloned()
}

/// A docked strip (`dockStrip` in `VelaWorkspace.ts`). Covering, the strip leaves its home
/// for a direct child slot of the root (`[data-vela-cover]`, grid area over the chart
/// column and the drawing toolbar) and a comment keeps its place in the home.
struct Strip {
    el: Element,
    home: Element,
    root: Element,
    doc: web_sys::Document,
    placeholder: RefCell<Option<web_sys::Comment>>,
}

impl Strip {
    fn uncover(&self) {
        let Some(placeholder) = self.placeholder.borrow_mut().take() else {
            return;
        };
        let _ = self.el.remove_attribute("data-vela-cover");
        let parent = placeholder.parent_node();
        if self.el.parent_element().is_some_and(|p| p == self.root)
            && let Some(parent) = &parent
        {
            let _ = parent.replace_child(&self.el, &placeholder);
        } else {
            placeholder.remove();
        }
    }
}

impl DockedStrip for Strip {
    fn undock(&self) {
        self.uncover();
        if self.el.parent_element().is_some_and(|p| p == self.home) {
            self.el.remove();
        }
    }
    fn cover(&self, on: bool) {
        if !on {
            return self.uncover();
        }
        if self.placeholder.borrow().is_some()
            || !self.el.parent_element().is_some_and(|p| p == self.home)
        {
            return;
        }
        let placeholder = self.doc.create_comment("vela-strip");
        let _ = self.home.replace_child(&placeholder, &self.el);
        *self.placeholder.borrow_mut() = Some(placeholder);
        attr(&self.el, "data-vela-cover", "");
        let _ = self.root.append_child(&self.el);
    }
}

impl WidgetContext for RootContext {
    fn chart(&self) -> Vela {
        self.ws()
            .and_then(|w| w.chart())
            .expect("vela: the workspace has no active chart")
    }
    fn symbol(&self) -> String {
        self.ws().map(|w| w.context().symbol()).unwrap_or_default()
    }
    fn timeframe(&self) -> String {
        self.ws()
            .map_or_else(|| "60".into(), |w| w.context().timeframe())
    }
    fn price_style(&self) -> String {
        self.ws()
            .map_or_else(|| "candles".into(), |w| w.context().price_style())
    }
    fn set_symbol(&self, symbol: &str) {
        if let Some(w) = self.ws() {
            w.context().set_symbol(symbol);
        }
    }
    fn set_timeframe(&self, tf: &str) {
        if let Some(w) = self.ws() {
            w.context().set_timeframe(tf);
        }
    }
    fn set_price_style(&self, style: &str) {
        if let Some(w) = self.ws() {
            w.context().set_price_style(style);
        }
    }
    fn open_symbol_search(&self, query: Option<&str>) {
        (self.open_symbol_search)(query);
    }
    fn toggle_panel(&self, id: &str, open: Option<bool>) {
        (self.toggle_panel)(id, open);
    }
    fn dock_strip(&self, el: ElementHandle, opts: DockStripOptions) -> Box<dyn DockedStrip> {
        let root = self.root();
        let element = element_of(&el).unwrap_or_else(|| {
            let doc = root
                .as_ref()
                .map(|r| r.document().clone())
                .or_else(crate::dom::global_document)
                .expect("a document");
            make(&doc, "div", "")
        });
        let below = opts.position == DockPosition::BelowBottombar;
        let (home, root_el, doc) = match &root {
            Some(root) => (
                root.region(if below {
                    Region::StripsBelow
                } else {
                    Region::Strips
                })
                .clone(),
                root.element().clone(),
                root.document().clone(),
            ),
            None => {
                let doc = crate::dom::global_document().expect("a document");
                (make(&doc, "div", ""), make(&doc, "div", ""), doc)
            }
        };
        let _ = home.append_child(&element);
        Box::new(Strip {
            el: element,
            home,
            root: root_el,
            doc,
            placeholder: RefCell::new(None),
        })
    }
    fn dock_edge(&self, el: ElementHandle, _side: DockEdge) -> Unsubscribe {
        let Some(element) = element_of(&el) else {
            return Box::new(|| {});
        };
        let edge = self.root().map(|root| root.region(Region::Edge).clone());
        if let Some(edge) = &edge {
            let _ = edge.append_child(&element);
        }
        // Only while the owner has not taken the element elsewhere.
        Box::new(move || {
            if let Some(edge) = edge
                && element.parent_element().is_some_and(|p| p == edge)
            {
                element.remove();
            }
        })
    }
    fn host(&self) -> ElementHandle {
        let el = self
            .root()
            .map(|r| r.element().clone())
            .expect("vela: the widget root is gone");
        Rc::new(el) as Rc<dyn Any>
    }
    fn i18n(&self) -> Rc<dyn Translator> {
        self.ws().expect("vela: the workspace is gone").i18n()
    }
    fn toast(&self, message: &str, kind: ToastKind) {
        let kind = match kind {
            ToastKind::Info => vela_core::workspace::ToastKind::Info,
            ToastKind::Success => vela_core::workspace::ToastKind::Success,
            ToastKind::Error => vela_core::workspace::ToastKind::Error,
        };
        if let Some(root) = self.root() {
            root.toast(message, kind, 3000.0);
        }
    }
    fn add_indicator(&self, entry: ExternalIndicatorEntry) {
        if let Some(w) = self.ws() {
            w.context().add_indicator(CellEntry {
                name: entry.name,
                script: entry.script,
                language: entry.language,
                category: None,
                id: entry.id,
                inputs: entry.inputs,
                props: entry.props,
                hidden: entry.hidden.unwrap_or(false),
            });
        }
    }
    fn add_native_indicator(&self, indicator_type: &str) {
        if let Some(w) = self.ws() {
            w.context().add_native_indicator(indicator_type);
        }
    }
    fn state_changed(&self) {
        if let Some(w) = self.ws() {
            w.context().state_changed();
        }
    }
    fn topbar(&self) -> &dyn TopbarItems {
        &*self.topbar
    }
    fn panels(&self) -> &dyn PanelDockView {
        &*self.panels
    }
    fn sidebar(&self) -> &dyn SidebarControl {
        &*self.sidebar
    }
    fn pointer(&self) -> Option<ContextMenuPointer> {
        self.pointer.clone()
    }
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}
