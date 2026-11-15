//! The widget root: the DOM skeleton of the shell and the services its components share.
//! See the module documentation of [`crate::workspace_ui`] for the contract.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use js_sys::Reflect;
use vela_core::i18n::Translator;
use vela_core::options::ThemeSpec;
use vela_core::ports::chart_renderer::{LayoutMode as RendererLayoutMode, VelaTheme};
use vela_core::runtime::{Platform, Unsubscribe};
use vela_core::ui::keymap::{
    KeyEvent, KeyEventTarget, KeymapManager, KeymapOptions, KeymapPlatform,
};
use vela_core::vela::VelaEnv;
use vela_core::widget::contributions::{Contributions, WidgetAttachment, WidgetContext};
use vela_core::widget::layout_mode::{LayoutMode, LayoutModeController, LayoutModeOption};
use vela_core::widget::shell_options::VelaShellOptions;
use vela_core::workspace::{ToastKind, WeakWorkspace, Workspace};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, KeyboardEvent};

use crate::dom::{Listener, attr, default_view, make, set_styles};
use crate::host::build_attribution_mark;

use super::context::{ContextSlots, RootContext};
use super::css;
use super::js;
use super::registry;
use super::splitters::SplitterLayer;
use super::storage::LocalStorageAdapter;
use super::toast::ToastView;
use super::tokens;

/// A named mount point of the widget root. The element of a region carries the original's
/// class name (listed per variant), `vela-ws-region` and `data-vela-region="<name>"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    /// `.vela-widget-topbar`: the row under the top edge, across the chart column.
    Topbar,
    /// `.vela-ws-toolbar`: the drawing toolbar's column, left of the chart column, from
    /// under the topbar to the foot. Hidden on the mobile layout.
    Toolbar,
    /// `.vela-ws-main`: the row that holds the workspace grid. The side-panel dock mounts
    /// here, beside the grid (the original's `PanelDock` takes this element as its host).
    Main,
    /// `.vela-ws-strips`: docked strips above the bottom bar (`ctx.dockStrip`).
    Strips,
    /// `.vela-widget-bottombar`: the bottom bar (range chips, clock, timezone, session).
    Bottombar,
    /// `.vela-ws-strips.vela-ws-strips-below`: strips under the bottom bar.
    StripsBelow,
    /// `.vela-mobilebar`: the foot bar of the mobile size class, across the full width.
    MobileBar,
    /// `.vela-ws-column`: a full-height column panel right of the chart column.
    Column,
    /// `.vela-ws-edge`: the right edge, full height (the sidebar mounts here).
    Edge,
}

impl Region {
    /// Every region, in DOM order.
    pub const ALL: [Region; 9] = [
        Region::Topbar,
        Region::Toolbar,
        Region::Main,
        Region::Strips,
        Region::Bottombar,
        Region::StripsBelow,
        Region::MobileBar,
        Region::Column,
        Region::Edge,
    ];

    /// The `data-vela-region` value.
    pub fn name(self) -> &'static str {
        match self {
            Region::Topbar => "topbar",
            Region::Toolbar => "toolbar",
            Region::Main => "main",
            Region::Strips => "strips",
            Region::Bottombar => "bottombar",
            Region::StripsBelow => "strips-below",
            Region::MobileBar => "mobilebar",
            Region::Column => "column",
            Region::Edge => "edge",
        }
    }

    /// The region from its [`name`](Self::name).
    pub fn from_name(name: &str) -> Option<Region> {
        Region::ALL.into_iter().find(|r| r.name() == name)
    }

    /// The class names of the region's element (the original's, plus `vela-ws-region`).
    pub fn class(self) -> &'static str {
        match self {
            Region::Topbar => "vela-widget-topbar vela-ws-region",
            Region::Toolbar => "vela-ws-toolbar vela-ws-region",
            Region::Main => "vela-ws-main",
            Region::Strips => "vela-ws-strips vela-ws-region",
            Region::Bottombar => "vela-widget-bottombar vela-ws-region",
            Region::StripsBelow => "vela-ws-strips vela-ws-strips-below vela-ws-region",
            Region::MobileBar => "vela-mobilebar vela-ws-region",
            Region::Column => "vela-ws-column vela-ws-region",
            Region::Edge => "vela-ws-edge vela-ws-region",
        }
    }
}

/// What the root is built from.
pub struct RootOptions {
    pub theme: ThemeSpec,
    pub layout_mode: LayoutModeOption,
    /// The shell options the widget was given, for the components to read.
    pub shell: VelaShellOptions,
}

impl Default for RootOptions {
    fn default() -> Self {
        Self {
            theme: ThemeSpec::from("dark"),
            layout_mode: LayoutModeOption::Auto,
            shell: VelaShellOptions::default(),
        }
    }
}

/// A piece of DOM mounted into a region; dropping the guard removes it.
pub struct Mount {
    el: Element,
}

impl Mount {
    pub fn element(&self) -> &Element {
        &self.el
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.el.remove();
    }
}

type ThemeListener = Rc<dyn Fn(&VelaTheme)>;
type ModeListener = Rc<dyn Fn(LayoutMode)>;
type RefreshHook = Rc<dyn Fn()>;

/// What the root keeps of a mounted attachment: the descriptor it mounted and its disposer.
type MountedAttachment = (Rc<WidgetAttachment>, Unsubscribe);

/// The widget root. See the module documentation of [`crate::workspace_ui`].
pub struct WidgetRoot {
    me: Weak<WidgetRoot>,
    doc: Document,
    platform: Rc<dyn Platform>,
    env: VelaEnv,
    root: Element,
    grid: Element,
    regions: HashMap<Region, Element>,
    mark: Element,
    toast: Rc<ToastView>,
    splitters: Rc<SplitterLayer>,
    storage: Rc<LocalStorageAdapter>,
    keymap: Rc<KeymapManager>,
    shell: VelaShellOptions,
    ws: RefCell<Option<WeakWorkspace>>,
    theme: RefCell<VelaTheme>,
    cells: RefCell<HashMap<String, Element>>,
    layout_ctl: LayoutModeController,
    theme_listeners: RefCell<Vec<(u64, ThemeListener)>>,
    mode_listeners: RefCell<Vec<(u64, ModeListener)>>,
    next_listener: Cell<u64>,
    disposers: RefCell<Vec<Unsubscribe>>,
    listeners: RefCell<Vec<Listener>>,
    observers: RefCell<Vec<JsValue>>,
    // Kept alive for the observers and watchers that call them.
    closures: RefCell<Vec<Closure<dyn FnMut()>>>,
    stops: RefCell<Vec<js_sys::Function>>,
    strips_pending: Cell<bool>,
    /// Whether the host chose a direction: `dir` is stamped on the root only then.
    dir_explicit: Cell<bool>,
    destroyed: Cell<bool>,
    contributions: Contributions,
    slots: ContextSlots,
    refresh_hooks: RefCell<Vec<(u64, RefreshHook)>>,
    attachments: RefCell<Vec<MountedAttachment>>,
    scripting: RefCell<Option<Rc<dyn std::any::Any>>>,
}

fn key_event_of(ev: &KeyboardEvent) -> KeyEvent {
    let target = ev
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .map(|el| KeyEventTarget {
            tag_name: el.tag_name(),
            is_content_editable: Reflect::get(&el, &JsValue::from_str("isContentEditable"))
                .ok()
                .is_some_and(|v| v.is_truthy()),
            role: el.get_attribute("role"),
        });
    KeyEvent {
        key: ev.key(),
        code: ev.code(),
        ctrl: ev.ctrl_key(),
        meta: ev.meta_key(),
        alt: ev.alt_key(),
        shift: ev.shift_key(),
        target,
        ..KeyEvent::default()
    }
}

fn renderer_mode(mode: LayoutMode) -> RendererLayoutMode {
    match mode {
        LayoutMode::Mobile => RendererLayoutMode::Mobile,
        LayoutMode::Desktop => RendererLayoutMode::Desktop,
    }
}

impl WidgetRoot {
    /// Build the root inside `container` (an element with a size: the root fills it).
    pub fn new(
        container: &Element,
        platform: Rc<dyn Platform>,
        env: VelaEnv,
        opts: RootOptions,
    ) -> Result<Rc<WidgetRoot>, JsValue> {
        let doc = container
            .owner_document()
            .or_else(crate::dom::global_document)
            .ok_or_else(|| JsValue::from_str("vela: no document to build the workspace in"))?;
        css::ensure_styles(&doc);
        let theme = env.registries.themes.borrow().resolve(Some(&opts.theme));

        let root = make(&doc, "div", "vela-workspace vela-ui");
        attr(&root, "data-vela-regions", "");
        attr(&root, "tabindex", "-1"); // a focusable host so bare keystrokes land here
        tokens::apply_theme(&root, &theme);
        let mut regions = HashMap::new();
        for region in Region::ALL {
            let el = make(&doc, "div", region.class());
            attr(&el, "data-vela-region", region.name());
            regions.insert(region, el);
        }
        let grid = make(&doc, "div", "vela-ws-grid");
        let main = regions[&Region::Main].clone();
        let _ = main.append_child(&grid);
        for region in Region::ALL {
            let _ = root.append_child(&regions[&region]);
        }

        // ONE attribution mark for the whole grid; it moves into the bottom-left cell.
        let mark = build_attribution_mark(&doc, &crate::dialogs::Tr::default());
        set_styles(
            &mark,
            &[
                ("left", "calc(var(--vela-toolbar-gutter, 0px) + 12px)"),
                ("bottom", "calc(var(--vela-bottom-gutter, 22px) + 10px)"),
                ("z-index", "11"),
            ],
        );
        let _ = grid.append_child(&mark);
        Self::ink_mark(&mark, &theme);

        let toast = ToastView::new(&doc, &grid, platform.clone());
        let keymap = Rc::new(KeymapManager::new(KeymapOptions {
            platform: Some(if js::vela_ws_platform_is_mac() {
                KeymapPlatform::Mac
            } else {
                KeymapPlatform::Other
            }),
            base_scope: None,
        }));
        let _ = container.append_child(&root);

        // Measured AFTER the root is in the DOM so the first evaluation sees a real width.
        let layout_ctl = LayoutModeController::new(
            opts.layout_mode,
            root.get_bounding_client_rect().width(),
            js::vela_ws_coarse_now(),
        );
        attr(&root, "data-layout", layout_ctl.current().as_str());
        attr(&root, "data-vela-layout", layout_ctl.current().as_str());

        let me_slot: Rc<RefCell<Weak<WidgetRoot>>> = Rc::new(RefCell::new(Weak::new()));
        let link: super::splitters::WorkspaceLink = {
            let slot = me_slot.clone();
            Rc::new(move || slot.borrow().upgrade()?.workspace())
        };
        let splitters = SplitterLayer::new(&doc, &grid, link);

        let this = Rc::new_cyclic(|me: &Weak<WidgetRoot>| WidgetRoot {
            me: me.clone(),
            doc: doc.clone(),
            platform,
            env,
            root,
            grid,
            regions,
            mark,
            toast,
            splitters,
            storage: Rc::new(LocalStorageAdapter::new()),
            keymap,
            shell: opts.shell,
            ws: RefCell::new(None),
            theme: RefCell::new(theme),
            cells: RefCell::default(),
            layout_ctl,
            theme_listeners: RefCell::default(),
            mode_listeners: RefCell::default(),
            next_listener: Cell::new(1),
            disposers: RefCell::default(),
            listeners: RefCell::default(),
            observers: RefCell::default(),
            closures: RefCell::default(),
            stops: RefCell::default(),
            strips_pending: Cell::new(false),
            dir_explicit: Cell::new(false),
            destroyed: Cell::new(false),
            contributions: registry::global_contributions(),
            slots: ContextSlots::default(),
            refresh_hooks: RefCell::default(),
            attachments: RefCell::default(),
            scripting: RefCell::default(),
        });
        *me_slot.borrow_mut() = Rc::downgrade(&this);
        this.wire();
        Ok(this)
    }

    fn wire(&self) {
        // Keyboard: one keymap at the root; the bindings are the components'.
        {
            let me = self.me.clone();
            self.listeners.borrow_mut().push(Listener::new(
                self.root.as_ref(),
                "keydown",
                false,
                false,
                move |ev: JsValue| {
                    let Some(root) = me.upgrade() else { return };
                    let ev: &KeyboardEvent = ev.unchecked_ref();
                    let key = key_event_of(ev);
                    if root.keymap.handle_keydown(&key) {
                        if key.default_prevented.get() {
                            ev.prevent_default();
                        }
                        if key.propagation_stopped.get() {
                            ev.stop_propagation();
                        }
                    }
                },
            ));
        }
        // The layout mode follows the root's width (and a coarse pointer): the container is
        // the honest signal in an embeddable widget.
        {
            let me = self.me.clone();
            let cb = Closure::<dyn FnMut()>::new(move || {
                if let Some(root) = me.upgrade() {
                    root.evaluate_layout_mode();
                }
            });
            let observer = js::vela_ws_observe(&self.root, cb.as_ref().unchecked_ref());
            self.observers.borrow_mut().push(observer);
            let stop = js::vela_ws_watch_coarse(cb.as_ref().unchecked_ref());
            self.stops.borrow_mut().push(stop);
            self.closures.borrow_mut().push(cb);
        }
        {
            let me = self.me.clone();
            let _ = self.layout_ctl.on_change(move |mode| {
                if let Some(root) = me.upgrade() {
                    root.on_mode_changed(mode);
                }
            });
        }
        // The splitter strips follow the grid's size.
        {
            let me = self.me.clone();
            let cb = Closure::<dyn FnMut()>::new(move || {
                if let Some(root) = me.upgrade() {
                    root.splitters.layout();
                }
            });
            let observer = js::vela_ws_observe(&self.grid, cb.as_ref().unchecked_ref());
            self.observers.borrow_mut().push(observer);
            self.closures.borrow_mut().push(cb);
        }
        // A page unload gets the same flush as a teardown: the pending edit reaches the
        // storage before the page goes.
        if let Some(view) = default_view(&self.doc) {
            let me = self.me.clone();
            self.listeners.borrow_mut().push(Listener::new(
                &view,
                "pagehide",
                false,
                true,
                move |_| {
                    if let Some(ws) = me.upgrade().and_then(|r| r.workspace()) {
                        ws.on_unload();
                    }
                },
            ));
        }
        {
            let me = self.me.clone();
            let doc_target: web_sys::EventTarget = self.doc.clone().into();
            self.listeners.borrow_mut().push(Listener::new(
                &doc_target,
                "visibilitychange",
                false,
                true,
                move |_| {
                    let Some(root) = me.upgrade() else { return };
                    if js::vela_ws_visibility(root.doc.as_ref()) == "hidden"
                        && let Some(ws) = root.workspace()
                    {
                        ws.on_unload();
                    }
                },
            ));
        }
    }

    fn evaluate_layout_mode(&self) {
        let width = self.root.get_bounding_client_rect().width();
        self.layout_ctl.evaluate(width, js::vela_ws_coarse_now());
    }

    fn on_mode_changed(&self, mode: LayoutMode) {
        attr(&self.root, "data-layout", mode.as_str());
        // The chart dialogs mount at the root and their sheet reads this attribute.
        attr(&self.root, "data-vela-layout", mode.as_str());
        if let Some(ws) = self.workspace() {
            ws.set_layout_mode(renderer_mode(mode));
        }
        let listeners: Vec<ModeListener> = self
            .mode_listeners
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for f in listeners {
            f(mode);
        }
    }

    /// Re-read the shared attribution mark's tooltip in the workspace's language.
    pub(crate) fn relabel_mark(&self) {
        let tr = crate::dialogs::Tr::new(self.translator());
        crate::host::relabel_attribution_mark(&self.mark, &tr);
    }

    fn ink_mark(mark: &Element, theme: &VelaTheme) {
        let dark = tokens::is_dark(theme);
        set_styles(
            mark,
            &[
                ("color", if dark { "#ffffff" } else { "#000000" }),
                (
                    "--vela-attr-shadow",
                    if dark {
                        "rgba(0,0,0,0.45)"
                    } else {
                        "rgba(255,255,255,0.55)"
                    },
                ),
            ],
        );
    }

    // ── structure ──

    /// The root element (`div.vela-workspace.vela-ui`, `data-layout`, `data-vela-regions`).
    pub fn element(&self) -> &Element {
        &self.root
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The element of a region (empty until a component mounts into it).
    pub fn region(&self, region: Region) -> &Element {
        &self.regions[&region]
    }

    /// The workspace grid (`div.vela-ws-grid`, a child of [`Region::Main`]): the cells, the
    /// splitter strips, the toast and the attribution mark live in it. Do not add children
    /// that take layout room; overlays that are `position: absolute` are fine.
    pub fn grid(&self) -> &Element {
        &self.grid
    }

    /// Mount `el` as the last child of `region`; dropping the guard removes it.
    pub fn mount(&self, region: Region, el: &Element) -> Mount {
        let _ = self.regions[&region].append_child(el);
        Mount { el: el.clone() }
    }

    /// The container of cell `id` (`div.vela-cell[data-cell-id]`), while the cell lives.
    /// Per-cell overlays (the statusline, the watermark, the cell controls) mount into it.
    pub fn cell_container(&self, id: &str) -> Option<Element> {
        self.cells.borrow().get(id).cloned()
    }

    /// The live cell containers in no particular order.
    pub fn cell_containers(&self) -> Vec<(String, Element)> {
        self.cells
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub(crate) fn register_cell(&self, id: &str, el: &Element) {
        self.cells.borrow_mut().insert(id.to_string(), el.clone());
    }

    pub(crate) fn forget_cell(&self, id: &str) {
        self.cells.borrow_mut().remove(id);
    }

    // ── the workspace ──

    pub(crate) fn set_workspace(&self, ws: &Workspace) {
        *self.ws.borrow_mut() = Some(ws.downgrade());
    }

    /// The workspace, once it exists (it does for every component: they mount after the
    /// workspace was built) and until it is destroyed.
    pub fn workspace(&self) -> Option<Workspace> {
        self.ws.borrow().as_ref()?.upgrade()
    }

    /// A non-owning handle to keep in closures.
    pub fn weak_workspace(&self) -> Option<WeakWorkspace> {
        self.ws.borrow().clone()
    }

    // ── contributions and the context ──

    /// The contribution registries of the widget: the page-wide ones the JavaScript
    /// `register*` functions fill. The topbar, the sidebar, the dock, the context menu and
    /// the legends of every cell read them (live: [`refresh_actions`](Self::refresh_actions)
    /// re-projects after a late registration).
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }

    /// The parts of the context the shell components fill when they mount.
    pub fn slots(&self) -> &ContextSlots {
        &self.slots
    }

    /// A fresh [`WidgetContext`] (the workspace's, plus the chrome members): the only
    /// implementation, shared by the topbar, the side surfaces and the cells' legends.
    /// Build one per invocation; reads are live.
    pub fn context(&self) -> Rc<RootContext> {
        let me = self.me.upgrade().expect("vela: the widget root is gone");
        // After `destroy` (a late call from a script that kept its context) the handle is
        // dead: every read of the context then answers its default instead of trapping.
        let ws = self
            .ws
            .borrow()
            .clone()
            .unwrap_or_else(WeakWorkspace::dangling);
        Rc::new(RootContext::new(&me, ws))
    }

    /// [`context`](Self::context) as the factory contributions take.
    pub fn context_factory(&self) -> Rc<dyn Fn() -> Rc<dyn WidgetContext>> {
        let me = self.me.clone();
        Rc::new(move || {
            let root = me.upgrade().expect("vela: the widget root is gone");
            root.context() as Rc<dyn WidgetContext>
        })
    }

    pub(crate) fn script_slot(&self) -> &RefCell<Option<Rc<dyn std::any::Any>>> {
        &self.scripting
    }

    /// Call `cb` on every [`refresh_actions`](Self::refresh_actions): the components that
    /// project contributions re-read them. Returns the disposer.
    pub fn on_refresh_actions(&self, cb: impl Fn() + 'static) -> Unsubscribe {
        let id = self.next_listener.get();
        self.next_listener.set(id + 1);
        self.refresh_hooks.borrow_mut().push((id, Rc::new(cb)));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(root) = me.upgrade() {
                root.refresh_hooks.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Re-project the contributions: mount attachments registered since, tell the components
    /// to redraw their contributed entries and bind every cell's legend providers.
    pub fn refresh_actions(&self) {
        if self.destroyed.get() || self.ws.borrow().is_none() {
            return;
        }
        self.mount_attachments();
        let hooks: Vec<RefreshHook> = self
            .refresh_hooks
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for f in hooks {
            f();
        }
        if let Some(ws) = self.workspace() {
            for cell in ws.cells() {
                if let Some(chart) = cell.try_chart() {
                    self.bind_legends(&chart);
                }
            }
        }
    }

    /// The legend providers of the registered legend actions and callouts, on one chart.
    pub(crate) fn bind_legends(&self, chart: &vela_core::vela::Vela) {
        let factory = self.context_factory();
        chart.renderer().set_legend_actions(Some(
            self.contributions
                .legend_actions_provider_for(chart, factory.clone()),
        ));
        chart.renderer().set_legend_callouts(Some(
            self.contributions
                .legend_callouts_provider_for(chart, factory),
        ));
    }

    /// Mount the registered attachments not yet mounted on this widget (idempotent per id).
    fn mount_attachments(&self) {
        for att in self.contributions.widget_attachments() {
            if self
                .attachments
                .borrow()
                .iter()
                .any(|(a, _)| a.id == att.id)
            {
                continue;
            }
            let ctx = self.context();
            let dispose = (att.mount)(&*ctx);
            self.attachments.borrow_mut().push((att, dispose));
        }
    }

    // ── services ──

    /// The workspace's translator (the new one after a language change: ask again).
    pub fn translator(&self) -> Option<Rc<dyn Translator>> {
        Some(self.workspace()?.i18n())
    }

    pub fn platform(&self) -> &Rc<dyn Platform> {
        &self.platform
    }

    pub fn env(&self) -> &VelaEnv {
        &self.env
    }

    /// The chart theme in force (the shell tokens on the root derive from it).
    pub fn theme(&self) -> VelaTheme {
        self.theme.borrow().clone()
    }

    /// Re-read the workspace's theme and rewrite the tokens, the mark and the seam color.
    /// Core's `Workspace::set_theme` already themed every cell; this is the chrome's half.
    pub fn refresh_theme(&self) {
        if let Some(ws) = self.workspace() {
            let theme = self
                .env
                .registries
                .themes
                .borrow()
                .resolve(Some(&ws.theme()));
            self.set_theme(theme);
        }
    }

    /// Write `theme`'s tokens on the root and tell the theme listeners.
    pub fn set_theme(&self, theme: VelaTheme) {
        if *self.theme.borrow() == theme {
            return;
        }
        tokens::apply_theme(&self.root, &theme);
        Self::ink_mark(&self.mark, &theme);
        *self.theme.borrow_mut() = theme.clone();
        let listeners: Vec<ThemeListener> = self
            .theme_listeners
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for f in listeners {
            f(&theme);
        }
    }

    /// Call `cb` whenever the theme tokens were rewritten. Returns the disposer.
    pub fn on_theme(&self, cb: impl Fn(&VelaTheme) + 'static) -> Unsubscribe {
        let id = self.next_listener.get();
        self.next_listener.set(id + 1);
        self.theme_listeners.borrow_mut().push((id, Rc::new(cb)));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(root) = me.upgrade() {
                root.theme_listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// The chrome size class (`data-layout` on the root).
    pub fn layout_mode(&self) -> LayoutMode {
        self.layout_ctl.current()
    }

    /// Call `cb` on every layout mode change (after `data-layout` and the cells followed).
    pub fn on_layout_mode(&self, cb: impl Fn(LayoutMode) + 'static) -> Unsubscribe {
        let id = self.next_listener.get();
        self.next_listener.set(id + 1);
        self.mode_listeners.borrow_mut().push((id, Rc::new(cb)));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(root) = me.upgrade() {
                root.mode_listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// `localStorage` through both storage contracts. Components persist their own state
    /// through the workspace document where they can; this is for the rest.
    pub fn storage(&self) -> &Rc<LocalStorageAdapter> {
        &self.storage
    }

    /// The keymap at the root: register descriptors, push scopes. The root feeds it every
    /// `keydown` that reaches it.
    pub fn keymap(&self) -> &Rc<KeymapManager> {
        &self.keymap
    }

    /// Show a notice on the grid's status pill.
    pub fn toast(&self, message: &str, kind: ToastKind, duration_ms: f64) {
        self.toast.show(message, kind, duration_ms);
    }

    /// The shell options the widget was created with.
    pub fn shell_options(&self) -> &VelaShellOptions {
        &self.shell
    }

    /// Run `f` after the current call stack unwinds (a microtask). The way to do work that
    /// calls the workspace after a [`WorkspaceHost`](vela_core::workspace::WorkspaceHost)
    /// method asked for it.
    pub fn defer(&self, f: impl FnOnce() + 'static) {
        self.platform.spawn(Box::pin(async move { f() }));
    }

    /// Register a cleanup that runs when the root is destroyed (in reverse order).
    pub fn on_dispose(&self, f: impl FnOnce() + 'static) {
        self.disposers.borrow_mut().push(Box::new(f));
    }

    /// Whether the host chose a direction (the root carries `dir` only then).
    pub fn dir_explicit(&self) -> bool {
        self.dir_explicit.get()
    }

    pub(crate) fn set_dir_explicit(&self, explicit: bool) {
        self.dir_explicit.set(explicit);
    }

    /// Set or clear `lang` and `dir` on the root (the host page decides when unset).
    pub fn set_locale_attributes(&self, lang: Option<&str>, dir: Option<&str>) {
        match lang {
            Some(l) => attr(&self.root, "lang", l),
            None => {
                let _ = self.root.remove_attribute("lang");
            }
        }
        match dir {
            Some(d) => attr(&self.root, "dir", d),
            None => {
                let _ = self.root.remove_attribute("dir");
            }
        }
    }

    // ── grid presentation (the host's) ──

    /// Re-lay out the splitter strips once the current call returned.
    pub(crate) fn schedule_strips(&self) {
        if self.strips_pending.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.defer(move || {
            if let Some(root) = me.upgrade() {
                root.strips_pending.set(false);
                if !root.destroyed.get() {
                    root.splitters.layout();
                }
            }
        });
    }

    /// Move the grid's single attribution mark into `host` (a cell container) or back into
    /// the grid.
    pub(crate) fn place_mark(&self, host: Option<&Element>) {
        let target = host.unwrap_or(&self.grid);
        let inside = self.mark.parent_element().is_some_and(|p| &p == target);
        if !inside {
            let _ = target.append_child(&self.mark);
        }
    }

    /// The grid's seam color: the computed background of the grid, the theme's border
    /// color when the browser cannot compute it.
    pub(crate) fn seam_color(&self) -> String {
        let computed = js::vela_ws_computed_background(&self.grid);
        if computed.is_empty() {
            tokens::seam_color(&self.theme.borrow())
        } else {
            computed
        }
    }

    // ── teardown ──

    /// Remove everything the root put in the page: components' cleanups, listeners,
    /// observers, strips, the toast, the keymap and the DOM. Idempotent.
    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        let mounted: Vec<MountedAttachment> = self.attachments.borrow_mut().drain(..).collect();
        for (_, dispose) in mounted {
            dispose();
        }
        let disposers: Vec<Unsubscribe> = self.disposers.borrow_mut().drain(..).rev().collect();
        for d in disposers {
            d();
        }
        self.refresh_hooks.borrow_mut().clear();
        *self.scripting.borrow_mut() = None;
        for stop in self.stops.borrow_mut().drain(..) {
            let _ = stop.call0(&JsValue::NULL);
        }
        for observer in self.observers.borrow_mut().drain(..) {
            js::vela_ws_disconnect(&observer);
        }
        self.listeners.borrow_mut().clear();
        self.closures.borrow_mut().clear();
        self.splitters.destroy();
        self.toast.destroy();
        self.keymap.destroy();
        self.layout_ctl.destroy();
        self.theme_listeners.borrow_mut().clear();
        self.mode_listeners.borrow_mut().clear();
        self.mark.remove();
        self.root.remove();
        *self.ws.borrow_mut() = None;
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed.get()
    }
}
