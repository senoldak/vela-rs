//! The SIDE of the widget shell: the right edge (sidebar), the docked side panels (object
//! tree, data window, contributed panels), the chart context menu, the keyboard (default
//! bindings, type-to-search routing, the shortcuts panel, the zoom glide) and the mobile
//! surfaces (the foot bar and its drawers, the drawing pill).
//!
//! | original | here |
//! |----------|------|
//! | `sidebar.ts` | [`sidebar`] |
//! | `side-panel.ts`, `panel-dock.ts` | [`dock`] |
//! | `object-tree.ts` | [`object_tree`] |
//! | `data-window.ts` | [`data_window`] |
//! | `context-menu.ts` | [`context_menu`] (+ the list in [`menu`]) |
//! | `shortcuts-help.ts`, the default keys of `VelaWorkspace.ts`, `glide.ts` | [`help`], [`keys`] |
//! | `mobile-bar.ts`, `more-drawer.ts`, `drawings-drawer.ts`, `price-scale-drawer.ts`, `drawing-pill.ts` | [`mobile`] |
//!
//! The logic of each is `vela_core::widget::*`; this module is its DOM skin. Entry point:
//! [`mount`] (one call per widget root; the returned [`ShellSide`] owns everything and removes
//! it on [`ShellSide::destroy`] or when the root is destroyed).
//!
//! # Integration points for the other shell parts
//!
//! - **Intents.** What the keyboard and the mobile surfaces ask of surfaces this module does not
//!   own (the symbol search, the timeframe entry, the indicator picker, the timeframe drawer,
//!   the alerts) goes through [`Intents`]: an [`Intent`] handed to the installed
//!   [`ShellHooks`] (set with [`ShellSide::set_hooks`]) AND dispatched as a bubbling
//!   `CustomEvent` named `vela:shell-intent` on the root (`detail = { intent, text }`).
//!   A host that is not wired yet loses nothing observable.
//! - **Dialog state.** The symbol search, the timeframe entry and other dialogs tell the
//!   router when they are up ([`ShellSide::set_dialog_open`], [`set_symbol_search_open`],
//!   [`set_timeframe_entry_open`]) so bare typing is handed on to them as the original does.
//! - **The panel toggles.** [`ShellSide::dock`] is the dock: a topbar shows the panel group
//!   from `dock.list()` / `dock.attach_chrome(..)` and toggles with `dock.toggle(id, None)`.
//! - **The context.** The widget has ONE `WidgetContext` (`WidgetRoot::context()`); this part
//!   fills its slots (the dock, the sidebar, the symbol search). `ctx.topbar()` is a stand-in
//!   until the topbar installs its own (`WidgetRoot::slots().set_topbar`, or
//!   [`ShellSide::set_topbar_items`]).
//! - **Surfaces the mobile bar reads** ([`MobileHost`]): symbol and timeframe stops, the
//!   topbar's contributed actions, the alerts list.
//!
//! # The dock's state
//!
//! The dock's `PanelDockModel` holds the one `SidePanelModel` of each panel (resize, maximize,
//! pin); a restored document's `panels` slice reaches the dock through the workspace's
//! `panels:restored` event, raised by `apply_state` after the whole document was applied.

pub mod bind;
pub mod context_menu;
pub mod css;
pub mod data_window;
pub mod dock;
pub mod help;
pub mod icons;
pub mod js;
pub mod keys;
pub mod kit;
pub mod menu;
pub mod mobile;
pub mod object_tree;
pub mod sidebar;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vela_core::widget::contributions::{Contributions, TopbarItems};
use vela_core::widget::shell_defaults::SidebarOption;
use vela_core::workspace::{WorkspaceEvent, WorkspaceEventName};
use wasm_bindgen::{JsCast, JsValue};

use crate::dom::Listener;
use crate::workspace_ui::{RootContext, WidgetRoot};

use self::dock::{Dock, DockView};
use self::kit::{Cx, Disposers};

/// What the side surfaces ask of surfaces owned elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Open the symbol search (`None`), or the context's `open_symbol_search(query)`.
    SymbolSearch(Option<String>),
    /// A bare letter or `0` was typed: open the symbol search seeded with this text.
    TypeSymbol(String),
    /// A bare digit 1-9 was typed: open the timeframe entry seeded with this text.
    TypeTimeframe(String),
    /// `/`: open the indicator picker.
    IndicatorPicker,
    /// The mobile bar's timeframe stop: open the timeframe drawer.
    TimeframeDrawer,
    /// The more drawer's alerts row.
    Alerts,
}

impl Intent {
    fn parts(&self) -> (&'static str, String) {
        match self {
            Intent::SymbolSearch(q) => ("symbol-search", q.clone().unwrap_or_default()),
            Intent::TypeSymbol(t) => ("type-symbol", t.clone()),
            Intent::TypeTimeframe(t) => ("type-timeframe", t.clone()),
            Intent::IndicatorPicker => ("indicator-picker", String::new()),
            Intent::TimeframeDrawer => ("timeframe-drawer", String::new()),
            Intent::Alerts => ("alerts", String::new()),
        }
    }
}

/// Implemented by the shell part that owns the surfaces an [`Intent`] names.
pub trait ShellHooks {
    fn on_intent(&self, intent: &Intent);
}

/// One row of the more drawer that comes from the topbar: label, icon id, what it runs.
pub type MobileAction = (String, Option<String>, Rc<dyn Fn()>);

/// What the mobile surfaces read from the rest of the shell (the topbar's state): the
/// symbol and timeframe shown on the bar, the topbar's contributed actions for the menu.
/// All optional: without it the bar shows the active cell's own values.
pub trait MobileHost {
    /// Rows of the more drawer that come from the topbar's contributed actions.
    fn topbar_actions(&self) -> Vec<MobileAction> {
        Vec::new()
    }
}

/// The intent router and the dialog counters the typing router reads.
pub struct Intents {
    root: Rc<WidgetRoot>,
    hooks: RefCell<Option<Rc<dyn ShellHooks>>>,
    dialogs: Cell<usize>,
    symbol: Cell<bool>,
    timeframe: Cell<bool>,
}

impl Intents {
    fn new(root: &Rc<WidgetRoot>) -> Rc<Self> {
        Rc::new(Self {
            root: root.clone(),
            hooks: RefCell::new(None),
            dialogs: Cell::new(0),
            symbol: Cell::new(false),
            timeframe: Cell::new(false),
        })
    }

    pub fn emit(&self, intent: Intent) {
        let hooks = self.hooks.borrow().clone();
        if let Some(h) = hooks {
            h.on_intent(&intent);
        }
        let (name, text) = intent.parts();
        js::vela_ss_emit(self.root.element(), "vela:shell-intent", name, &text);
    }

    pub fn dialogs(&self) -> usize {
        self.dialogs.get()
    }

    pub fn symbol_open(&self) -> bool {
        self.symbol.get()
    }

    pub fn timeframe_open(&self) -> bool {
        self.timeframe.get()
    }

    /// A dialog opened or closed (counts, and pushes / pops nothing: the caller owns scopes).
    pub fn dialog_opened(&self, open: bool) {
        let n = self.dialogs.get();
        self.dialogs
            .set(if open { n + 1 } else { n.saturating_sub(1) });
    }
}

/// What a mount is given beyond the root.
#[derive(Default)]
pub struct MountOptions {
    /// The contribution registries (side panels, sidebar and context-menu actions). Default:
    /// the root's own, which the JavaScript `register*` functions fill.
    pub contributions: Option<Contributions>,
    /// The owner of the surfaces the intents name.
    pub hooks: Option<Rc<dyn ShellHooks>>,
}

struct Inner {
    cx: Cx,
    intents: Rc<Intents>,
    dock: Rc<Dock>,
    sidebar: RefCell<Option<Rc<sidebar::Sidebar>>>,
    data_window: Rc<data_window::DataWindow>,
    object_tree: Rc<object_tree::ObjectTree>,
    menus: RefCell<Option<Rc<context_menu::ContextMenus>>>,
    help: Rc<help::ShortcutsHelp>,
    glide: Rc<keys::GlideLoop>,
    mobile: RefCell<Option<Rc<mobile::Mobile>>>,
    disposers: Disposers,
    listeners: RefCell<Vec<Listener>>,
    destroyed: Cell<bool>,
}

/// The owner of everything this module put on the page.
#[derive(Clone)]
pub struct ShellSide {
    inner: Option<Rc<Inner>>,
}

/// Mount the side surfaces on `root` (the workspace must exist).
pub fn mount(root: &Rc<WidgetRoot>) -> ShellSide {
    mount_with(root, MountOptions::default())
}

/// [`mount`] with the widget's contributions and the intent hooks.
pub fn mount_with(root: &Rc<WidgetRoot>, mut opts: MountOptions) -> ShellSide {
    // The root's registries (the page-wide ones `registerSidePanel`, `registerWidgetAction`...
    // fill) unless the host brings its own.
    if opts.contributions.is_none() {
        opts.contributions = Some(root.contributions().clone());
    }
    let Some(ws) = root.workspace() else {
        return ShellSide { inner: None };
    };
    css::ensure_styles(root.document());
    let cx = Cx::new(root);
    let intents = Intents::new(root);
    *intents.hooks.borrow_mut() = opts.hooks;
    let disposers = Disposers::default();
    let dock = Dock::new(&cx, opts.contributions.clone());

    // The sidebar exists before the dock: the dock publishes the built-in panels' toggles as
    // it docks them. A host's own value wins over a plugin's default (no registry here).
    let composition = root
        .shell_options()
        .sidebar
        .as_ref()
        .and_then(sidebar::composition_of);
    let sidebar = composition.map(|c| {
        let sb = sidebar::Sidebar::new(&cx, c, &dock, opts.contributions.clone());
        let a = Rc::downgrade(&sb);
        let b = Rc::downgrade(&sb);
        dock.attach_chrome(
            Rc::new(move |buttons| {
                if let Some(s) = a.upgrade() {
                    s.set_panel_buttons(buttons);
                }
            }),
            Rc::new(move |id, open| {
                if let Some(s) = b.upgrade() {
                    s.set_panel_active(id, open);
                }
            }),
        );
        // `data-vela-sidebar` is a bare attribute in the original.
        let _ = root.element().set_attribute("data-vela-sidebar", "");
        sb
    });

    // The context handed to contributions is the root's ONE context; this part fills its
    // slots: the dock, the sidebar and the symbol search.
    let slots = root.slots();
    {
        let (toggle, open_id) = (Rc::downgrade(&dock), intents.clone());
        slots.set_dock(
            Rc::new(DockView(Rc::downgrade(&dock))),
            Rc::new(move |id, open| {
                if let Some(d) = toggle.upgrade() {
                    d.toggle(id, open);
                }
            }),
        );
        slots.set_symbol_search(Rc::new(move |query| {
            open_id.emit(Intent::SymbolSearch(query.map(str::to_string)));
        }));
    }
    if let Some(sb) = &sidebar {
        slots.set_sidebar(sb.control());
    }
    let make_ctx: Rc<dyn Fn() -> Rc<RootContext>> = {
        let root = Rc::downgrade(root);
        Rc::new(move || {
            root.upgrade()
                .expect("vela: the widget root is gone")
                .context()
        })
    };
    let ctx_dyn = root.context_factory();
    dock.set_context_factory(ctx_dyn.clone());
    if let Some(sb) = &sidebar {
        sb.set_context_factory(ctx_dyn.clone());
    }

    let data_window = data_window::DataWindow::new(&cx, &dock);
    let object_tree = object_tree::ObjectTree::new(&cx, &dock);
    dock.refresh();
    // The dock's slice of the boot document.
    if let Some(p) = ws.panels_state() {
        dock.apply_state(Some(&p));
    }
    // Every docked panel follows the active cell.
    {
        let d = Rc::downgrade(&dock);
        bind::follow_active(&cx, &ws, &disposers, move |chart| {
            if let (Some(d), Some((id, chart))) = (d.upgrade(), chart) {
                d.on_chart(id, chart);
            }
        });
    }
    // A restored document carries its own `panels`: `apply_state` raises `panels:restored`
    // once the document is applied, and the dock converges at once (a document without the
    // slice leaves the dock as it stands, as the original's `applyState(undefined)` does).
    {
        let d = Rc::downgrade(&dock);
        let w = ws.downgrade();
        let sub = ws.on(WorkspaceEventName::PanelsRestored, move |_| {
            let (Some(d), Some(ws)) = (d.upgrade(), w.upgrade()) else {
                return;
            };
            if let Some(doc) = ws.panels_state() {
                d.apply_state(Some(&doc));
            }
        });
        let w = ws.downgrade();
        disposers.push(move || {
            if let Some(ws) = w.upgrade() {
                ws.off(sub);
            }
        });
    }

    // Keyboard.
    let help = help::ShortcutsHelp::new(&cx, &intents);
    let glide = keys::GlideLoop::new(&ws);
    {
        let h = Rc::downgrade(&help);
        keys::register_defaults(
            &cx,
            &ws,
            &intents,
            &glide,
            Rc::new(move || {
                if let Some(h) = h.upgrade() {
                    h.open();
                }
            }),
            &disposers,
        );
    }
    keys::push_tool_hints(&cx, &ws);
    {
        // Hints follow cells created later and a language change.
        let (c, w) = (cx.clone(), ws.downgrade());
        for name in [
            WorkspaceEventName::CellCreated,
            WorkspaceEventName::LocaleChanged,
        ] {
            let (c, w2) = (c.clone(), w.clone());
            let sub = ws.on(name, move |_| {
                if let Some(ws) = w2.upgrade() {
                    keys::push_tool_hints(&c, &ws);
                }
            });
            let w3 = w.clone();
            disposers.push(move || {
                if let Some(ws) = w3.upgrade() {
                    ws.off(sub);
                }
            });
        }
    }
    let mut listeners = vec![keys::route_typing(&cx, &intents)];
    // A mid-glide cell switch must not steer the next cell's viewport.
    {
        let g = Rc::downgrade(&glide);
        let sub = ws.on(WorkspaceEventName::CellActive, move |_| {
            if let Some(g) = g.upgrade() {
                g.stop();
            }
        });
        let w = ws.downgrade();
        disposers.push(move || {
            if let Some(ws) = w.upgrade() {
                ws.off(sub);
            }
        });
    }

    let menus = context_menu::ContextMenus::new(
        &cx,
        &ws,
        opts.contributions.clone(),
        make_ctx.clone(),
        &disposers,
    );

    // Language: panels, sidebar and the toggles re-read their words.
    {
        let (d, sb, dw, ot) = (
            Rc::downgrade(&dock),
            sidebar.as_ref().map(Rc::downgrade),
            Rc::downgrade(&data_window),
            Rc::downgrade(&object_tree),
        );
        let w = ws.downgrade();
        let sub = ws.on(
            WorkspaceEventName::LocaleChanged,
            move |ev: &WorkspaceEvent| {
                let _ = ev;
                if let Some(d) = d.upgrade() {
                    d.relocalize();
                }
                if let Some(s) = sb.as_ref().and_then(|s| s.upgrade()) {
                    s.relocalize();
                }
                if let Some(x) = dw.upgrade() {
                    x.relocalize();
                }
                if let Some(x) = ot.upgrade() {
                    x.relocalize();
                }
            },
        );
        disposers.push(move || {
            if let Some(ws) = w.upgrade() {
                ws.off(sub);
            }
        });
    }

    let inner = Rc::new(Inner {
        cx: cx.clone(),
        intents: intents.clone(),
        dock,
        sidebar: RefCell::new(sidebar.clone()),
        data_window,
        object_tree,
        menus: RefCell::new(Some(menus)),
        help,
        glide,
        mobile: RefCell::new(None),
        disposers,
        listeners: RefCell::new(std::mem::take(&mut listeners)),
        destroyed: Cell::new(false),
    });

    // The mobile surfaces replace the desktop parts on the mobile layout.
    let mobile = mobile::Mobile::new(
        &cx,
        &ws,
        &inner.intents,
        &inner.dock,
        &inner.disposers,
        opts.contributions.clone(),
    );
    *inner.mobile.borrow_mut() = Some(mobile);

    if let Some(sb) = &sidebar {
        sb.activate();
    }
    // A late registration re-projects: the contributed panels, the sidebar entries and the
    // mobile menu re-read the registries.
    {
        let weak = Rc::downgrade(&inner);
        let off = root.on_refresh_actions(move || {
            let Some(i) = weak.upgrade() else { return };
            if i.destroyed.get() {
                return;
            }
            i.dock.refresh();
            if let Some(sb) = i.sidebar.borrow().clone() {
                sb.render();
            }
            if let Some(m) = i.mobile.borrow().clone() {
                m.refresh();
            }
        });
        root.on_dispose(off);
    }
    let side = ShellSide { inner: Some(inner) };
    let again = side.clone();
    root.on_dispose(move || again.destroy());
    side
}

/// The `sidebar` option from a JavaScript options object: `true`, `false`, or a composition
/// `{ top: [...], bottom: [...] }`.
pub fn sidebar_option_of(options: &JsValue) -> Option<SidebarOption> {
    use vela_core::widget::sidebar_composition::SidebarComposition;
    let value = js_sys::Reflect::get(options, &JsValue::from_str("sidebar")).ok()?;
    if value.is_undefined() || value.is_null() {
        return None;
    }
    if let Some(b) = value.as_bool() {
        return Some(SidebarOption::Enabled(b));
    }
    if !value.is_object() {
        return None;
    }
    let list = |key: &str| -> Option<Vec<String>> {
        let v = js_sys::Reflect::get(&value, &JsValue::from_str(key)).ok()?;
        let arr = v.dyn_ref::<js_sys::Array>()?;
        Some(arr.iter().filter_map(|x| x.as_string()).collect())
    };
    Some(SidebarOption::Composition(SidebarComposition {
        top: list("top"),
        bottom: list("bottom"),
    }))
}

impl ShellSide {
    /// The panel dock (for the topbar's panel group and `ctx.toggle_panel`).
    pub fn dock(&self) -> Option<Rc<Dock>> {
        self.inner.as_ref().map(|i| i.dock.clone())
    }

    /// `ctx.panels` over the dock.
    pub fn dock_view(&self) -> Option<DockView> {
        self.inner
            .as_ref()
            .map(|i| DockView(Rc::downgrade(&i.dock)))
    }

    /// Install the owner of the surfaces the intents name.
    pub fn set_hooks(&self, hooks: Rc<dyn ShellHooks>) {
        if let Some(i) = &self.inner {
            *i.intents.hooks.borrow_mut() = Some(hooks);
        }
    }

    pub fn intents(&self) -> Option<Rc<Intents>> {
        self.inner.as_ref().map(|i| i.intents.clone())
    }

    /// A dialog (symbol search, timeframe entry, ...) opened or closed. Pushes / pops the
    /// keymap's `dialog` scope like the original's `trackDialog`.
    pub fn set_dialog_open(&self, open: bool) {
        if let Some(i) = &self.inner {
            i.intents.dialog_opened(open);
            if open {
                i.cx.root.keymap().push_scope("dialog");
            } else {
                i.cx.root.keymap().pop_scope("dialog");
            }
        }
    }

    /// A dialog of another part opened or closed, which pushed the keymap's `dialog` scope
    /// itself: only the typing router's counter moves.
    pub fn count_dialog(&self, open: bool) {
        if let Some(i) = &self.inner {
            i.intents.dialog_opened(open);
        }
    }

    pub fn set_symbol_search_open(&self, open: bool) {
        if let Some(i) = &self.inner {
            i.intents.symbol.set(open);
        }
    }

    pub fn set_timeframe_entry_open(&self, open: bool) {
        if let Some(i) = &self.inner {
            i.intents.timeframe.set(open);
        }
    }

    /// Replace the stand-in `ctx.topbar()` with the real topbar's visibility (the root's one
    /// context: every part sees it).
    pub fn set_topbar_items(&self, items: Rc<dyn TopbarItems>) {
        if let Some(i) = &self.inner {
            i.cx.root.slots().set_topbar(items);
        }
    }

    /// Let the mobile surfaces read the topbar's state.
    pub fn set_mobile_host(&self, host: Rc<dyn MobileHost>) {
        if let Some(i) = &self.inner
            && let Some(m) = i.mobile.borrow().as_ref()
        {
            m.set_host(host);
        }
    }

    /// The sidebar, when the widget has one.
    pub fn sidebar(&self) -> Option<Rc<sidebar::Sidebar>> {
        self.inner.as_ref().and_then(|i| i.sidebar.borrow().clone())
    }

    /// The shortcuts panel.
    pub fn open_shortcuts(&self) {
        if let Some(i) = &self.inner {
            i.help.open();
        }
    }

    /// Remove everything: DOM, listeners, subscriptions. Idempotent.
    pub fn destroy(&self) {
        let Some(i) = &self.inner else { return };
        if i.destroyed.replace(true) {
            return;
        }
        i.disposers.run();
        let taken = i.mobile.borrow_mut().take();
        if let Some(m) = taken {
            m.destroy();
        }
        let taken = i.menus.borrow_mut().take();
        if let Some(m) = taken {
            m.destroy();
        }
        i.help.destroy();
        i.glide.stop();
        i.listeners.borrow_mut().clear();
        i.data_window.destroy();
        i.object_tree.destroy();
        i.dock.destroy();
        let taken = i.sidebar.borrow_mut().take();
        if let Some(sb) = taken {
            sb.destroy();
        }
        let _ = i.root_attr_clear();
    }
}

impl Inner {
    fn root_attr_clear(&self) -> bool {
        let el = self.cx.root.element();
        let had = el.has_attribute("data-vela-sidebar");
        let _ = el.remove_attribute("data-vela-sidebar");
        had
    }
}
