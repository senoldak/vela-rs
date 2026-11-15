//! The top and the bottom of the widget shell, and the per-cell overlays: the browser skin of
//! the original's `topbar.ts`, `symbol-picker.ts`, `symbol-icon.ts`, `indicator-picker.ts`,
//! `layout-picker.ts`, `timeframe-quick.ts`, `timeframe-drawer.ts`, `timezone-drawer.ts`,
//! `bottombar.ts`, `statusline.ts`, `watermark.ts`, `cell-controls.ts` and the wiring
//! `VelaWorkspace.ts` gives them.
//!
//! The logic of every component is its model in `vela_core::widget`; this module puts it on
//! the page and wires it to the workspace:
//!
//! ```text
//! mount(root) ──► ShellTop (drop / destroy removes everything)
//!   ├─ Topbar            → Region::Topbar       (symbol, timeframes, style, layout,
//!   │                                            indicators, actions, undo/redo, alerts,
//!   │                                            panels, screenshot)
//!   ├─ Bottombar         → Region::Bottombar    (range chips, clock + zone, RTH/ETH, gear)
//!   ├─ SymbolPicker, IndicatorPicker, TimeframeQuick       dialogs on the root
//!   ├─ TimeframeDrawer, TimezoneDrawer                     the mobile sheets
//!   └─ CellOverlay per cell → statusline, watermark, hover controls, market badge and
//!                             session-shading trackers, inside `root.cell_container(id)`
//! ```
//!
//! Everything follows the ACTIVE cell through the workspace's `projection` event, the cells
//! through `cell:created` / `cell:destroyed`, and the language through `locale:changed`.
//!
//! # Integration points
//!
//! - The side-panel toggles of the topbar are filled by the dock:
//!   [`ShellTop::set_panel_buttons`] / [`ShellTop::set_panel_active`].
//! - Contributed actions, slot overrides and the symbol ranking come from the root's
//!   registries and its one context (`WidgetRoot::contributions` / `context_factory`), unless
//!   the [`TopHooks`] bring their own. `ctx.topbar` is installed in the root's slots.
//! - The side of the shell opens the pickers and the sheets through its intents and through
//!   [`ShellTop::open_symbol_search`], [`ShellTop::open_timeframe_drawer`],
//!   [`ShellTop::open_timezone_drawer`] and [`ShellTop::open_indicators`].
//! - [`ShellTop::set_item_visible`] / [`ShellTop::item_visible`] are the runtime hiding of
//!   built-in topbar items (`ctx.topbar`).
//!
//! # Differences from the original
//!
//! - The kit (dialog, menu, tooltip, tabs, drawer) is native elements (`kit`).
//! - The default keyboard set, `/` and the bare-typing router belong to `shell_side::keys`,
//!   which asks for the surfaces of this module through the root's `vela:shell-intent` event
//!   (see `Shell::listen_intents`); the screenshot chord (`mod+alt+s`) belongs to the
//!   screenshot slot and is registered here while the slot shows. The dialogs of this module
//!   tell the router through [`TopHooks::on_dialog`] and its two siblings.
//! - A status-prefs change on any cell reaches its overlay through the workspace's
//!   `cell:statusPrefs` event.

#![allow(clippy::type_complexity)]

pub mod bottombar;
pub mod cell_controls;
pub mod cells;
pub mod css;
pub mod icons;
pub mod indicator_picker;
pub mod js;
pub mod kit;
pub mod layout_picker;
pub mod quick;
pub mod statusline;
pub mod symbol_picker;
pub mod topbar;
pub mod watermark;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use vela_core::i18n::{MessageArg, MessageLike, msg, text_of};
use vela_core::runtime::Unsubscribe;
use vela_core::state::SyncKind;
use vela_core::ui::MenuItemDescriptor;
use vela_core::ui::keymap::KeyBindingDescriptor;
use vela_core::widget::bottombar::RangePreset;
use vela_core::widget::contributions::{
    ContextFactory, Contributions, MobilePlacement, SidePanelButton, TopbarItemId, TopbarItems,
    WidgetActionTarget, mobile_placement,
};
use vela_core::widget::indicator_picker::IndicatorRow;
use vela_core::widget::layout_mode::LayoutMode;
use vela_core::widget::layout_picker::{LayoutPickerShape, PickerRow};
use vela_core::widget::topbar::{indicators_message, screenshot_message};
use vela_core::widget::topbar_composition::{
    TOPBAR_BUILTIN_IDS, TopbarComposition, resolve_topbar_composition, topbar_has,
};
use vela_core::workspace::cell::CellRangePreset;
use vela_core::workspace::layouts::{layout_glyph, layout_shape};
use vela_core::workspace::{Workspace, WorkspaceEvent, WorkspaceEventName, WorkspaceSubscription};
use wasm_bindgen::JsValue;
use web_sys::{Element, EventTarget};

use crate::dom::{Listener, attr};
use crate::workspace_ui::{Mount, Region, WidgetRoot};

use bottombar::{Bottombar, BottombarOpts};
use cells::{CellHooks, CellOverlay};
use indicator_picker::{IndicatorPicker, IndicatorPickerOpts};
use kit::Env;
use quick::{TimeframeDrawer, TimeframeDrawerOpts, TimeframeQuick, TimezoneDrawer};
use symbol_picker::{SymbolPicker, SymbolPickerOpts};
use topbar::{Topbar, TopbarLayout, TopbarOpts};

/// The timeframe presets of the topbar's dropdown and the mobile sheet.
pub const DEFAULT_TIMEFRAMES: [&str; 9] = ["1", "5", "15", "30", "60", "240", "D", "W", "M"];

/// What the host of the shell supplies beyond the root (all optional).
#[derive(Clone, Default)]
pub struct TopHooks {
    /// The widget runs in single-chart mode: no layout button, no sync switches.
    pub single_chart: bool,
    /// The registries contributed actions, slot overrides and the symbol ranking live in,
    /// with the factory of the context an action runs against. Default: the root's own
    /// registries and its one context.
    pub contributions: Option<(Contributions, ContextFactory)>,
    /// The PNG data URL of what the screenshot tool downloads (default: the workspace's own
    /// `screenshot()`, which composites one cell and a grid alike).
    pub screenshot: Option<Rc<dyn Fn() -> Option<String>>>,
    /// A dialog or sheet of this module opened (`true`) or closed (`false`): the typing
    /// router's dialog counter. The module pushes the keymap's `dialog` scope itself.
    pub on_dialog: Option<Rc<dyn Fn(bool)>>,
    /// The symbol search opened or closed.
    pub on_symbol_search: Option<Rc<dyn Fn(bool)>>,
    /// The timeframe quick entry opened or closed.
    pub on_timeframe_entry: Option<Rc<dyn Fn(bool)>>,
}

/// An owner of everything this module put on the page: dropping it, or calling
/// [`destroy`](ShellTop::destroy), removes the DOM and the listeners.
pub struct ShellTop {
    shell: Rc<Shell>,
}

/// Mount the top shell on `root`.
pub fn mount(root: &Rc<WidgetRoot>) -> ShellTop {
    mount_with(root, TopHooks::default())
}

/// [`mount`] with the host's hooks.
pub fn mount_with(root: &Rc<WidgetRoot>, hooks: TopHooks) -> ShellTop {
    ShellTop {
        shell: Shell::build(root, hooks),
    }
}

impl Drop for ShellTop {
    fn drop(&mut self) {
        self.shell.destroy();
    }
}

impl ShellTop {
    /// Remove everything (idempotent).
    pub fn destroy(&self) {
        self.shell.destroy();
    }

    /// Open the symbol search, its field seeded with `query`.
    pub fn open_symbol_search(&self, query: &str) {
        self.shell.open_symbol_search(query);
    }

    /// Open the indicator picker (no-op when the composition or an override removed it).
    pub fn open_indicators(&self) {
        self.shell.open_indicators();
    }

    /// The mobile bar's timeframe sheet.
    pub fn open_timeframe_drawer(&self) {
        self.shell.open_timeframe_drawer();
    }

    /// The mobile time-zone sheet.
    pub fn open_timezone_drawer(&self) {
        self.shell.open_timezone_drawer();
    }

    /// Replace the side-panel toggles of the topbar (the dock's own order).
    pub fn set_panel_buttons(&self, buttons: &[SidePanelButton], on_click: Rc<dyn Fn(&str)>) {
        if let Some(t) = self.shell.topbar.borrow().as_ref() {
            t.set_panel_buttons(buttons, on_click);
        }
    }

    /// Reflect a docked panel's open state on its toggle.
    pub fn set_panel_active(&self, id: &str, open: bool) {
        if let Some(t) = self.shell.topbar.borrow().as_ref() {
            t.set_panel_active(id, open);
        }
    }

    /// Re-project the contributed topbar actions (after registrations changed).
    pub fn render_actions(&self) {
        if let Some(t) = self.shell.topbar.borrow().as_ref() {
            t.render_actions();
        }
    }

    /// Hide or show a built-in topbar item at runtime (`ctx.topbar`).
    pub fn set_item_visible(&self, id: TopbarItemId, visible: bool) {
        self.shell.set_item_visible(id, visible);
    }

    /// Whether a built-in item shows: in the composition, and not hidden at runtime.
    pub fn item_visible(&self, id: TopbarItemId) -> bool {
        self.shell.item_visible(id)
    }

    /// The topbar, while it exists.
    pub fn topbar(&self) -> Option<Rc<Topbar>> {
        self.shell.topbar.borrow().clone()
    }

    /// The bottom bar, while it exists.
    pub fn bottombar(&self) -> Option<Rc<Bottombar>> {
        self.shell.bottombar.borrow().clone()
    }

    /// The overlay of one cell, while it lives.
    pub fn cell_overlay(&self, id: &str) -> Option<Rc<CellOverlay>> {
        self.shell.overlays.borrow().get(id).cloned()
    }

    /// Dialogs and sheets this module has open right now.
    pub fn open_dialogs(&self) -> usize {
        self.shell.dialogs.get()
    }

    /// The topbar's contributed actions the mobile menu lists (menu-placed, `when`-gated):
    /// label, icon id and what a pick runs. Hand it to the side module's mobile surfaces.
    pub fn mobile_actions(&self) -> Rc<dyn Fn() -> Vec<(String, Option<String>, Rc<dyn Fn()>)>> {
        let weak = self.shell.me.clone();
        Rc::new(move || {
            weak.upgrade()
                .map(|s| s.mobile_actions())
                .unwrap_or_default()
        })
    }

    /// `ctx.topbar` over this topbar: the runtime visibility of the built-in items.
    pub fn items(&self) -> Rc<dyn TopbarItems> {
        Rc::new(TopItems(self.shell.me.clone()))
    }
}

/// `ctx.topbar`: reads and writes the shell's runtime item visibility.
struct TopItems(Weak<Shell>);

impl TopbarItems for TopItems {
    fn is_visible(&self, id: TopbarItemId) -> bool {
        self.0.upgrade().is_some_and(|s| s.item_visible(id))
    }
    fn set_visible(&self, id: TopbarItemId, visible: bool) {
        if let Some(s) = self.0.upgrade() {
            s.set_item_visible(id, visible);
        }
    }
}

fn picker_rows(rows: Vec<vela_core::workspace::SyncRow>) -> Vec<PickerRow> {
    rows.into_iter()
        .map(|r| PickerRow {
            id: r.id.to_string(),
            label: r.label,
            checked: r.checked,
        })
        .collect()
}

/// The shell: every component, the subscriptions and the dialog count.
struct Shell {
    me: Weak<Shell>,
    env: Env,
    hooks: TopHooks,
    composition: Option<TopbarComposition>,
    topbar: RefCell<Option<Rc<Topbar>>>,
    bottombar: RefCell<Option<Rc<Bottombar>>>,
    symbol: RefCell<Option<Rc<SymbolPicker>>>,
    indicators: RefCell<Option<Rc<IndicatorPicker>>>,
    quick: RefCell<Option<Rc<TimeframeQuick>>>,
    tf_drawer: RefCell<Option<Rc<TimeframeDrawer>>>,
    tz_drawer: RefCell<Option<Rc<TimezoneDrawer>>>,
    overlays: RefCell<HashMap<String, Rc<CellOverlay>>>,
    mounts: RefCell<Vec<Mount>>,
    subs: RefCell<Vec<WorkspaceSubscription>>,
    unsubs: RefCell<Vec<Unsubscribe>>,
    listeners: RefCell<Vec<Listener>>,
    hidden: RefCell<HashSet<&'static str>>,
    screenshot_key: RefCell<Option<Unsubscribe>>,
    /// The open alerts dropdown: its listeners live in the menu, so it is kept here (a dropped
    /// menu would stay on the page with nothing to dismiss or pick it).
    alerts_menu: RefCell<Option<Rc<kit::Menu>>>,
    timeframes: Vec<String>,
    dialogs: Cell<usize>,
    destroyed: Cell<bool>,
}

impl Shell {
    fn build(root: &Rc<WidgetRoot>, mut hooks: TopHooks) -> Rc<Shell> {
        css::ensure_styles(root.document());
        // The root's registries and its one context, unless the host brought its own.
        if hooks.contributions.is_none() && root.workspace().is_some() {
            hooks.contributions = Some((root.contributions().clone(), root.context_factory()));
        }
        let env = Env {
            root: Rc::downgrade(root),
            doc: root.document().clone(),
            platform: root.platform().clone(),
        };
        let options = root.shell_options();
        let timeframes = options
            .timeframes
            .clone()
            .unwrap_or_else(|| DEFAULT_TIMEFRAMES.iter().map(|s| s.to_string()).collect());
        let shell = Rc::new_cyclic(|me| Shell {
            me: me.clone(),
            env,
            composition: options.topbar.clone(),
            hooks,
            topbar: RefCell::new(None),
            bottombar: RefCell::new(None),
            symbol: RefCell::new(None),
            indicators: RefCell::new(None),
            quick: RefCell::new(None),
            tf_drawer: RefCell::new(None),
            tz_drawer: RefCell::new(None),
            overlays: RefCell::new(HashMap::new()),
            mounts: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
            unsubs: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            hidden: RefCell::new(HashSet::new()),
            screenshot_key: RefCell::new(None),
            alerts_menu: RefCell::new(None),
            timeframes,
            dialogs: Cell::new(0),
            destroyed: Cell::new(false),
        });
        if root.workspace().is_some() {
            shell.assemble(root);
        }
        shell
    }

    fn ws(&self) -> Option<Workspace> {
        self.env.root()?.workspace()
    }

    fn active_cell(&self) -> Option<vela_core::workspace::ChartCell> {
        self.ws()?.active()
    }

    fn composition(&self) -> vela_core::widget::topbar_composition::ResolvedTopbarComposition {
        resolve_topbar_composition(self.composition.as_ref())
    }

    fn indicators_override(&self) -> bool {
        self.hooks
            .contributions
            .as_ref()
            .is_some_and(|(c, _)| c.topbar_action_override("indicators").is_some())
    }

    // ── assembly ──

    fn assemble(self: &Rc<Self>, root: &Rc<WidgetRoot>) {
        let Some(ws) = root.workspace() else { return };
        let options = root.shell_options().clone();
        let comp = self.composition();
        let tr = |me: &Rc<Shell>| -> Rc<dyn Fn(bool)> {
            let w = Rc::downgrade(me);
            Rc::new(move |open| {
                if let Some(s) = w.upgrade() {
                    s.track_dialog(open);
                }
            })
        };

        // ── the pickers ──
        let me = Rc::downgrade(self);
        let (src_ws, ranking) = (ws.downgrade(), self.hooks.contributions.clone());
        let picker = SymbolPicker::new(
            &self.env,
            SymbolPickerOpts {
                on_select: {
                    let me = me.clone();
                    Rc::new(move |sym| {
                        if let Some(c) = me.upgrade().and_then(|s| s.active_cell()) {
                            c.set_symbol(sym);
                        }
                    })
                },
                on_open_change: {
                    let (me, track) = (me.clone(), tr(self));
                    let flag = self.hooks.on_symbol_search.clone();
                    Rc::new(move |open| {
                        // In-chart dialogs live inside a cell's chart container: opening the
                        // search from the topbar never hits their outside-dismiss, so close
                        // them on every cell explicitly.
                        if open && let Some(s) = me.upgrade() {
                            s.close_chart_dialogs();
                        }
                        if let Some(f) = &flag {
                            f(open);
                        }
                        track(open);
                    })
                },
                source: Rc::new(move || {
                    Rc::new(
                        src_ws
                            .upgrade()
                            .map(|w| w.feed().symbols(None))
                            .unwrap_or_default(),
                    )
                }),
                icon_for: {
                    let w = ws.downgrade();
                    Rc::new(move |d| w.upgrade()?.feed().symbol_icon_of(d))
                },
                ranking: Rc::new(move || ranking.as_ref().and_then(|(c, _)| c.symbol_ranking())),
            },
        );
        *self.symbol.borrow_mut() = Some(picker);

        let wants_indicators = options.indicator_picker != Some(false)
            && !self.indicators_override()
            && topbar_has(&comp, "indicators");
        if wants_indicators {
            let (m1, m2, m3, m4) = (me.clone(), me.clone(), me.clone(), me.clone());
            let track = tr(self);
            let picker = IndicatorPicker::new(
                &self.env,
                IndicatorPickerOpts {
                    library: Rc::new(move || {
                        m1.upgrade()
                            .and_then(|s| s.active_cell())
                            .map(|c| {
                                c.library_rows()
                                    .into_iter()
                                    .map(|r| IndicatorRow {
                                        name: r.name,
                                        aliases: r.aliases,
                                        language: r.language,
                                        category: r.category,
                                        native: r.native,
                                        native_type: r.native_type,
                                        beta: r.beta,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }),
                    on_chart: Rc::new(move || {
                        m2.upgrade()
                            .and_then(|s| s.active_cell())
                            .map(|c| {
                                c.on_chart_rows()
                                    .into_iter()
                                    .map(|r| IndicatorRow {
                                        name: r.name,
                                        aliases: r.aliases,
                                        language: r.language,
                                        category: None,
                                        native: r.native,
                                        native_type: r.native_type,
                                        beta: false,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }),
                    on_add: Rc::new(move |i| {
                        if let Some(c) = m3.upgrade().and_then(|s| s.active_cell()) {
                            c.add_from_library(i);
                        }
                    }),
                    on_remove: Rc::new(move |i| {
                        if let Some(c) = m4.upgrade().and_then(|s| s.active_cell()) {
                            c.remove_from_chart(i);
                        }
                    }),
                    on_open_change: {
                        let (me, track) = (me.clone(), track);
                        Rc::new(move |open| {
                            if open && let Some(s) = me.upgrade() {
                                s.close_chart_dialogs();
                            }
                            track(open);
                        })
                    },
                },
            );
            *self.indicators.borrow_mut() = Some(picker);
        }

        let quick = {
            let m = me.clone();
            let (track, flag) = (tr(self), self.hooks.on_timeframe_entry.clone());
            TimeframeQuick::new(
                &self.env,
                Rc::new(move |tf| {
                    if let Some(s) = m.upgrade() {
                        s.set_active_timeframe(tf);
                    }
                }),
                Rc::new(move |open| {
                    if let Some(f) = &flag {
                        f(open);
                    }
                    track(open);
                }),
            )
        };
        *self.quick.borrow_mut() = Some(quick);

        // ── the topbar ──
        let layout = (!self.hooks.single_chart).then(|| self.topbar_layout(&ws));
        let m = |f: fn(&Shell)| -> Rc<dyn Fn()> {
            let w = me.clone();
            Rc::new(move || {
                if let Some(s) = w.upgrade() {
                    f(&s);
                }
            })
        };
        let on_symbol = m(|s| s.open_symbol_search(""));
        let on_indicators = wants_indicators.then(|| m(|s| s.open_indicators()));
        let on_undo = m(|s| {
            if let Some(c) = s.active_cell() {
                c.history().undo();
            }
        });
        let on_redo = m(|s| {
            if let Some(c) = s.active_cell() {
                c.history().redo();
            }
        });
        let on_screenshot = m(|s| s.run_screenshot());
        let topbar = Topbar::new(
            &self.env,
            TopbarOpts {
                timeframes: self.timeframes.clone(),
                timeframe_favorites: ws.timeframe_favorites(),
                composition: self.composition.clone(),
                on_symbol_click: on_symbol,
                on_indicators_click: on_indicators,
                on_undo,
                on_redo,
                on_screenshot,
                on_alerts: {
                    let w = me.clone();
                    Rc::new(move |anchor: &Element| {
                        if let Some(s) = w.upgrade() {
                            s.open_alerts_menu(anchor);
                        }
                    })
                },
                on_timeframe: {
                    let w = me.clone();
                    Rc::new(move |tf| {
                        if let Some(s) = w.upgrade() {
                            s.set_active_timeframe(tf);
                        }
                    })
                },
                on_timeframe_favorite: Some({
                    let w = me.clone();
                    Rc::new(move |tf: &str, on: bool| {
                        if let Some(ws) = w.upgrade().and_then(|s| s.ws()) {
                            ws.set_timeframe_favorite(tf, on);
                        }
                    })
                }),
                on_price_style: {
                    let w = me.clone();
                    Rc::new(move |style| {
                        if let Some(c) = w.upgrade().and_then(|s| s.active_cell()) {
                            c.set_price_style(style);
                        }
                    })
                },
                layout,
                contributions: self.hooks.contributions.clone(),
            },
        );
        {
            let mount = root.mount(Region::Topbar, topbar.element());
            self.mounts.borrow_mut().push(mount);
        }
        *self.topbar.borrow_mut() = Some(topbar);

        // ── the bottom bar ──
        if options.bottombar != Some(false) {
            let (w1, w2, w3, w4) = (me.clone(), me.clone(), me.clone(), me.clone());
            let bar = Bottombar::new(
                &self.env,
                BottombarOpts {
                    timezone: ws.timezone(),
                    exchange_timezone: ws.active().and_then(|c| c.exchange_timezone()),
                    on_range: Rc::new(move |p: &RangePreset| {
                        if let Some(c) = w1.upgrade().and_then(|s| s.active_cell()) {
                            c.apply_range(&CellRangePreset {
                                id: p.id.to_string(),
                                tf: p.tf.to_string(),
                                bars: p.bars as usize,
                                preset: p.preset,
                            });
                        }
                    }),
                    on_timezone: Rc::new(move |zone| {
                        if let Some(ws) = w2.upgrade().and_then(|s| s.ws()) {
                            ws.set_timezone(zone);
                        }
                    }),
                    on_session: Rc::new(move |session| {
                        if let Some(c) = w3.upgrade().and_then(|s| s.active_cell()) {
                            c.set_session(session);
                        }
                    }),
                    on_settings: Rc::new(move || {
                        if let Some(ch) = w4
                            .upgrade()
                            .and_then(|s| s.active_cell())
                            .and_then(|c| c.try_chart())
                        {
                            ch.renderer().open_settings(None);
                        }
                    }),
                },
            );
            let mount = root.mount(Region::Bottombar, bar.element());
            self.mounts.borrow_mut().push(mount);
            *self.bottombar.borrow_mut() = Some(bar);
        }

        // `ctx.topbar` is this topbar; a late registration re-projects the contributed
        // actions; the screenshot chord belongs to the screenshot slot.
        root.slots()
            .set_topbar(Rc::new(TopItems(Rc::downgrade(self))));
        {
            let w = Rc::downgrade(self);
            let off = root.on_refresh_actions(move || {
                if let Some(t) = w.upgrade().and_then(|s| s.topbar.borrow().clone()) {
                    t.render_actions();
                }
            });
            self.unsubs.borrow_mut().push(off);
        }
        self.register_screenshot_key();

        self.subscribe(root, &ws);
        self.listen_intents(root);
        for cell in ws.cells() {
            self.attach_cell(cell.id());
        }
        self.sync_cell_presentation();
        self.project_active();
        let _ = (
            indicators_message,
            screenshot_message,
            msg("x", "x"),
            MessageArg::from(0usize),
        );
    }

    fn topbar_layout(self: &Rc<Self>, ws: &Workspace) -> TopbarLayout {
        let me = Rc::downgrade(self);
        let w = |me: &Weak<Shell>| {
            let me = me.clone();
            move || me.upgrade().and_then(|s| s.ws())
        };
        let (w1, w2, w3, w4, w5, w6, w7) = (w(&me), w(&me), w(&me), w(&me), w(&me), w(&me), w(&me));
        TopbarLayout {
            current: ws.layout().id,
            shape: Rc::new(move || {
                let def = w1()?.layout();
                layout_shape(&def).map(|s| LayoutPickerShape {
                    rows: s.rows,
                    cols: s.cols,
                })
            }),
            presets: Rc::new(move || {
                let Some(ws) = w2() else { return Vec::new() };
                let tr = ws.i18n();
                ws.layouts()
                    .layouts(Some(&*tr))
                    .into_iter()
                    .filter(|l| layout_shape(l).is_none())
                    .map(|l| (l.id, l.label))
                    .collect()
            }),
            on_select_grid: Rc::new(move |rows, cols| {
                let Some(ws) = w3() else { return };
                let tr = ws.i18n();
                let def = ws
                    .layouts()
                    .layout_for_grid(rows as f64, cols as f64, Some(&*tr));
                let _ = ws.set_layout(&def.id);
            }),
            on_select_preset: Rc::new(move |id| {
                if let Some(ws) = w4() {
                    let _ = ws.set_layout(id);
                }
            }),
            syncs: Rc::new(move || {
                w5().map(|ws| picker_rows(ws.sync_rows()))
                    .unwrap_or_default()
            }),
            on_toggle_sync: Rc::new(move |id| {
                if let (Some(ws), Some(kind)) =
                    (w6(), SyncKind::ALL.into_iter().find(|k| k.as_str() == id))
                {
                    ws.toggle_sync(kind);
                }
            }),
            glyph: Rc::new(move || {
                w7().map(|ws| layout_glyph(&ws.layout()))
                    .unwrap_or_default()
            }),
        }
    }

    // ── subscriptions ──

    fn subscribe(self: &Rc<Self>, root: &Rc<WidgetRoot>, ws: &Workspace) {
        use WorkspaceEventName as N;
        let mut subs = Vec::new();
        let mut on = |name: N, f: Rc<dyn Fn(&Rc<Shell>, &WorkspaceEvent)>| {
            let w = self.me.clone();
            subs.push(ws.on(name, move |ev| {
                if let Some(s) = w.upgrade()
                    && !s.destroyed.get()
                {
                    f(&s, ev);
                }
            }));
        };
        on(N::CellActive, Rc::new(|s, _| s.project_active()));
        on(
            N::Projection,
            Rc::new(|s, ev| {
                s.project_active();
                if let WorkspaceEvent::Projection { id } = ev {
                    let overlay = s.overlays.borrow().get(id).cloned();
                    if let Some(o) = overlay {
                        o.on_projection();
                    }
                }
            }),
        );
        on(
            N::CellCreated,
            Rc::new(|s, ev| {
                if let WorkspaceEvent::CellCreated { id } = ev {
                    s.attach_cell(id);
                    s.sync_cell_presentation();
                }
            }),
        );
        on(
            N::CellDestroyed,
            Rc::new(|s, ev| {
                if let WorkspaceEvent::CellDestroyed { id } = ev {
                    let o = s.overlays.borrow_mut().remove(id);
                    if let Some(o) = o {
                        o.destroy();
                    }
                    s.sync_cell_presentation();
                }
            }),
        );
        on(
            N::LayoutChanged,
            Rc::new(|s, _| {
                s.sync_layout();
                s.sync_cell_presentation();
            }),
        );
        on(N::CellMaximized, Rc::new(|s, _| s.refresh_controls()));
        on(N::SyncChanged, Rc::new(|s, _| s.sync_layout()));
        on(N::TimezoneChanged, Rc::new(|s, _| s.project_timezone()));
        on(
            N::FavoritesChanged,
            Rc::new(|s, _| {
                if let (Some(ws), Some(t)) = (s.ws(), s.topbar.borrow().as_ref()) {
                    t.set_timeframe_favorites(&ws.timeframe_favorites());
                }
            }),
        );
        on(N::AlertsChanged, Rc::new(|s, _| s.project_alerts()));
        on(
            N::StatusPrefs,
            Rc::new(|s, ev| {
                if let WorkspaceEvent::StatusPrefs { id } = ev {
                    let overlay = s.overlays.borrow().get(id).cloned();
                    if let Some(o) = overlay {
                        o.sync_prefs();
                    }
                }
            }),
        );
        on(
            N::StateChanged,
            Rc::new(|s, _| {
                let overlays: Vec<Rc<CellOverlay>> =
                    s.overlays.borrow().values().cloned().collect();
                for o in overlays {
                    o.sync_prefs();
                }
            }),
        );
        on(N::LocaleChanged, Rc::new(|s, _| s.relocalize()));
        *self.subs.borrow_mut() = subs;

        let w = self.me.clone();
        let unsub = root.on_layout_mode(move |mode| {
            if let Some(s) = w.upgrade() {
                s.on_layout_mode(mode);
            }
        });
        self.unsubs.borrow_mut().push(unsub);
    }

    /// The side of the shell asks for surfaces this module owns through the root's
    /// `vela:shell-intent` event (`detail = { intent, text }`): the symbol search (typed or
    /// asked for), the timeframe entry (typed), the indicator picker (`/`), the timeframe
    /// sheet and the alerts.
    fn listen_intents(self: &Rc<Self>, root: &Rc<WidgetRoot>) {
        let w = self.me.clone();
        let target: EventTarget = root.element().clone().into();
        let l = Listener::new(
            &target,
            "vela:shell-intent",
            false,
            false,
            move |ev: JsValue| {
                let Some(s) = w.upgrade() else { return };
                if s.destroyed.get() {
                    return;
                }
                let detail = crate::dom::get_prop(&ev, "detail");
                let (intent, text) = (
                    crate::dom::get_str(&detail, "intent"),
                    crate::dom::get_str(&detail, "text"),
                );
                s.on_intent(&intent, &text);
            },
        );
        self.listeners.borrow_mut().push(l);
    }

    fn on_intent(self: &Rc<Self>, intent: &str, text: &str) {
        match intent {
            "symbol-search" => self.open_symbol_search(text),
            "type-symbol" => {
                if let Some(p) = self.symbol.borrow().clone() {
                    p.type_text(text);
                }
            }
            "type-timeframe" => {
                if let Some(q) = self.quick.borrow().clone() {
                    q.type_text(text);
                }
            }
            "indicator-picker" => self.open_indicators(),
            "timeframe-drawer" => self.open_timeframe_drawer(),
            "timezone-drawer" => self.open_timezone_drawer(),
            "alerts" => {
                let anchor = self
                    .topbar
                    .borrow()
                    .as_ref()
                    .map(|t| t.alerts_button().clone());
                if let Some(a) = anchor.or_else(|| self.env.host()) {
                    self.open_alerts_menu(&a);
                }
            }
            _ => {}
        }
    }

    // ── dialogs ──

    fn track_dialog(&self, open: bool) {
        let n = self.dialogs.get();
        self.dialogs
            .set(if open { n + 1 } else { n.saturating_sub(1) });
        if let Some(f) = &self.hooks.on_dialog {
            f(open);
        }
        if let Some(root) = self.env.root() {
            if open {
                root.keymap().push_scope("dialog");
            } else {
                root.keymap().pop_scope("dialog");
            }
        }
    }

    fn close_chart_dialogs(&self) {
        if let Some(ws) = self.ws() {
            for cell in ws.cells() {
                if let Some(ch) = cell.try_chart() {
                    ch.renderer().close_dialogs();
                }
            }
        }
    }

    fn open_symbol_search(&self, query: &str) {
        if let Some(p) = self.symbol.borrow().clone() {
            p.open(query);
        }
    }

    fn open_indicators(&self) {
        if let Some(p) = self.indicators.borrow().clone() {
            p.open();
        }
    }

    fn open_timeframe_drawer(self: &Rc<Self>) {
        if self.tf_drawer.borrow().is_none() {
            let (a, b, c, d) = (
                self.me.clone(),
                self.me.clone(),
                self.me.clone(),
                self.me.clone(),
            );
            let track = {
                let w = self.me.clone();
                Rc::new(move |open| {
                    if let Some(s) = w.upgrade() {
                        s.track_dialog(open);
                    }
                })
            };
            let drawer = TimeframeDrawer::new(
                &self.env,
                TimeframeDrawerOpts {
                    timeframes: self.timeframes.clone(),
                    current_timeframe: Rc::new(move || {
                        a.upgrade()
                            .and_then(|s| s.active_cell())
                            .map(|c| c.timeframe())
                            .unwrap_or_default()
                    }),
                    active_range: Rc::new(move || {
                        b.upgrade()
                            .and_then(|s| s.active_cell())
                            .and_then(|c| c.active_range_id())
                    }),
                    on_timeframe: Rc::new(move |tf| {
                        if let Some(s) = c.upgrade() {
                            s.set_active_timeframe(tf);
                        }
                    }),
                    on_range: Rc::new(move |id| {
                        if let Some(s) = d.upgrade() {
                            s.apply_range_id(id);
                        }
                    }),
                    on_open_change: track,
                },
            );
            *self.tf_drawer.borrow_mut() = Some(drawer);
        }
        let drawer = self.tf_drawer.borrow().clone();
        if let Some(d) = drawer {
            d.open();
        }
    }

    fn open_timezone_drawer(self: &Rc<Self>) {
        if self.tz_drawer.borrow().is_none() {
            let (a, b) = (self.me.clone(), self.me.clone());
            let w = self.me.clone();
            let drawer = TimezoneDrawer::new(
                &self.env,
                Rc::new(move || {
                    a.upgrade()
                        .and_then(|s| s.ws())
                        .map(|w| w.timezone())
                        .unwrap_or_default()
                }),
                Rc::new(move |zone| {
                    if let Some(ws) = b.upgrade().and_then(|s| s.ws()) {
                        ws.set_timezone(zone);
                    }
                }),
                Rc::new(move |open| {
                    if let Some(s) = w.upgrade() {
                        s.track_dialog(open);
                    }
                }),
            );
            *self.tz_drawer.borrow_mut() = Some(drawer);
        }
        let drawer = self.tz_drawer.borrow().clone();
        if let Some(d) = drawer {
            d.open();
        }
    }

    // ── actions ──

    fn apply_range_id(&self, id: &str) {
        let Some(p) = vela_core::widget::bottombar::RANGE_PRESETS
            .iter()
            .find(|p| p.id == id)
        else {
            return;
        };
        if let Some(c) = self.active_cell() {
            c.apply_range(&CellRangePreset {
                id: p.id.to_string(),
                tf: p.tf.to_string(),
                bars: p.bars as usize,
                preset: p.preset,
            });
        }
        // The desktop bar stays truthful across a mode flip.
        if let Some(b) = self.bottombar.borrow().as_ref() {
            b.set_active_range(Some(p.id));
        }
    }

    /// A timeframe change from the dropdown, a chip or the quick entry.
    fn set_active_timeframe(&self, tf: &str) {
        if let Some(b) = self.bottombar.borrow().as_ref() {
            b.set_active_range(None);
        }
        if let Some(c) = self.active_cell() {
            c.set_timeframe(tf);
        }
    }

    /// The screenshot slot's action: an override takes over (its `when` respected), else the
    /// built-in download.
    fn run_screenshot(&self) {
        if let Some((contrib, factory)) = &self.hooks.contributions
            && let Some(ov) = contrib.topbar_action_override("screenshot")
        {
            let ctx = factory();
            if ov.when.as_ref().is_none_or(|w| w(ctx.as_ref())) {
                (ov.run)(ctx.as_ref());
            }
            return;
        }
        self.download_screenshot();
    }

    /// The screenshot chord (`mod+alt+s`), while the screenshot slot shows.
    fn register_screenshot_key(self: &Rc<Self>) {
        if self.screenshot_key.borrow().is_some() || !self.item_visible(TopbarItemId::Screenshot) {
            return;
        }
        let Some(root) = self.env.root() else { return };
        let label: MessageLike = self
            .hooks
            .contributions
            .as_ref()
            .and_then(|(c, _)| c.topbar_action_override("screenshot"))
            .map_or_else(
                || {
                    msg(
                        "workspace.keys.screenshot",
                        "Download a screenshot of the layout",
                    )
                },
                |ov| ov.label.clone(),
            );
        let me = self.me.clone();
        let mut d = KeyBindingDescriptor::new("chart.screenshot", "mod+alt+s", label, move |_| {
            if let Some(s) = me.upgrade() {
                s.run_screenshot();
            }
        });
        d.category = Some(msg("workspace.keys.category.chart", "Chart"));
        *self.screenshot_key.borrow_mut() = Some(root.keymap().register(d));
    }

    /// The contributed topbar actions the mobile menu lists: menu-placed, `when`-gated.
    fn mobile_actions(&self) -> Vec<(String, Option<String>, Rc<dyn Fn()>)> {
        let Some((contrib, factory)) = self.hooks.contributions.clone() else {
            return Vec::new();
        };
        let ctx = factory();
        let tr = self.env.translator();
        contrib
            .widget_actions(WidgetActionTarget::Topbar, Some(ctx.as_ref()))
            .into_iter()
            .filter(|a| {
                mobile_placement(a.align, a.mobile) == MobilePlacement::Menu
                    && !TOPBAR_BUILTIN_IDS.contains(&a.id.as_str())
            })
            .map(|a| {
                let label = text_of(tr.as_deref(), &a.label, &[]);
                let (run, factory) = (a.run.clone(), factory.clone());
                let pick: Rc<dyn Fn()> = Rc::new(move || {
                    let ctx = factory();
                    run(ctx.as_ref());
                });
                (label, a.icon.clone(), pick)
            })
            .collect()
    }

    fn download_screenshot(&self) {
        let Some(ws) = self.ws() else { return };
        let url = match &self.hooks.screenshot {
            Some(f) => f(),
            None => ws.screenshot(),
        };
        let Some(url) = url else { return };
        let visible = ws.maximized_cell().map_or(ws.cells().len(), |_| 1);
        let name = if visible > 1 {
            "vela-layout.png".to_string()
        } else {
            match ws.active() {
                Some(c) => format!(
                    "{}-{}.png",
                    if c.symbol().is_empty() {
                        "chart".to_string()
                    } else {
                        c.symbol()
                    },
                    c.timeframe()
                ),
                None => "chart.png".to_string(),
            }
        };
        crate::workspace_ui::js::vela_ws_download(
            &JsValue::from(self.env.doc.clone()),
            &url,
            &name,
        );
    }

    /// The aggregated alerts bell: entries carry their cell; selecting one activates it.
    fn open_alerts_menu(self: &Rc<Self>, anchor: &Element) {
        let Some(ws) = self.ws() else { return };
        let Some(root) = self.env.root() else { return };
        let alerts = ws.alerts();
        let intl = root.env().intl.clone();
        let tr = ws.i18n();
        let items: Vec<MenuItemDescriptor> = if alerts.is_empty() {
            vec![MenuItemDescriptor {
                disabled: true,
                ..MenuItemDescriptor::new(
                    "none",
                    self.env.t(&msg("workspace.alerts.none", "No alerts yet")),
                )
            }]
        } else {
            alerts
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let row = self.env.tv(
                        &msg(
                            "workspace.alerts.row",
                            "{source} \u{b7} {time} \u{b7} {title}: {message}",
                        ),
                        &[
                            ("source", MessageArg::from(a.source.clone())),
                            (
                                "time",
                                MessageArg::from(vela_core::i18n::text::event_time_text(
                                    a.time as f64,
                                    Some(&*tr),
                                    intl.as_ref(),
                                )),
                            ),
                            ("title", MessageArg::from(a.title.clone())),
                            ("message", MessageArg::from(a.message.clone())),
                        ],
                    );
                    MenuItemDescriptor::new(
                        i.to_string(),
                        vela_core::i18n::text::slice_graphemes(intl.as_ref(), &row, 80),
                    )
                })
                .collect()
        };
        let r = anchor.get_bounding_client_rect();
        let w = self.me.clone();
        if let Some(old) = self.alerts_menu.borrow_mut().take() {
            old.close();
        }
        let menu = topbar::open_menu_at(
            &self.env,
            (r.left(), r.bottom() + 4.0),
            items,
            Rc::new(move |id| {
                let Some(ws) = w.upgrade().and_then(|s| s.ws()) else {
                    return;
                };
                let Ok(i) = id.parse::<usize>() else { return };
                if let Some(a) = ws.alerts().get(i)
                    && ws.cell(&a.cell_id).is_some()
                {
                    ws.set_active_cell(Some(&a.cell_id));
                }
            }),
        );
        // The menu closes itself (outside press, Escape, a pick); this handle keeps it alive
        // until then.
        *self.alerts_menu.borrow_mut() = menu;
    }

    // ── projection ──

    /// The chrome rebinds to the active cell.
    fn project_active(&self) {
        let Some(ws) = self.ws() else { return };
        let Some(p) = ws.projection() else { return };
        if let Some(t) = self.topbar.borrow().as_ref() {
            t.set_symbol(&p.symbol);
            t.set_timeframe(&p.timeframe);
            t.set_price_style(&p.price_style, &p.unavailable_price_styles);
            t.set_indicator_count(p.indicator_count);
            t.set_history_state(p.can_undo, p.can_redo);
            t.render_actions(); // contributed `when()` gates may depend on the active cell
            t.set_layout(&ws.layout().id);
        }
        if let Some(b) = self.bottombar.borrow().as_ref() {
            b.set_active_range(p.active_range_id.as_deref());
            b.set_session(p.session, p.session_available);
            b.set_timezone(&p.timezone_choice, p.exchange_timezone.as_deref());
        }
        if let Some(i) = self.indicators.borrow().as_ref() {
            i.sync(); // the dialog may be open while the active cell changes
        }
    }

    fn project_timezone(&self) {
        let Some(ws) = self.ws() else { return };
        let exchange = ws.active().and_then(|c| c.exchange_timezone());
        if let Some(b) = self.bottombar.borrow().as_ref() {
            b.set_timezone(&ws.timezone(), exchange.as_deref());
        }
        let drawer = self.tz_drawer.borrow().clone();
        let _ = drawer;
    }

    fn project_alerts(&self) {
        if let (Some(ws), Some(t)) = (self.ws(), self.topbar.borrow().as_ref()) {
            t.set_alert_count(ws.alerts().len());
        }
    }

    fn sync_layout(&self) {
        let Some(ws) = self.ws() else { return };
        if let Some(t) = self.topbar.borrow().as_ref() {
            t.set_layout(&ws.layout().id);
        }
    }

    // ── cells ──

    fn attach_cell(self: &Rc<Self>, id: &str) {
        if self.overlays.borrow().contains_key(id) {
            return;
        }
        let (Some(root), Some(ws)) = (self.env.root(), self.ws()) else {
            return;
        };
        let Some(container) = root.cell_container(id) else {
            return;
        };
        let w = self.me.clone();
        let overlay = CellOverlay::attach(
            &self.env,
            &ws,
            id,
            &container,
            CellHooks {
                open_timezone_drawer: Rc::new(move || {
                    if let Some(s) = w.upgrade() {
                        s.open_timezone_drawer();
                    }
                }),
            },
        );
        if let Some(o) = overlay {
            o.set_mobile(root.layout_mode() == LayoutMode::Mobile);
            self.overlays.borrow_mut().insert(id.to_string(), o);
        }
    }

    /// Multi-cell grids keep the status line on one row; the maximize gate follows the
    /// cell count.
    fn sync_cell_presentation(&self) {
        let Some(ws) = self.ws() else { return };
        let multi = ws.cells().len() > 1;
        let overlays: Vec<Rc<CellOverlay>> = self.overlays.borrow().values().cloned().collect();
        for o in overlays {
            o.set_fit_mode(multi);
            o.controls.refresh();
        }
    }

    fn refresh_controls(&self) {
        let overlays: Vec<Rc<CellOverlay>> = self.overlays.borrow().values().cloned().collect();
        for o in overlays {
            o.controls.refresh();
        }
    }

    // ── language, size class ──

    fn relocalize(&self) {
        if let Some(t) = self.topbar.borrow().as_ref() {
            t.relocalize();
        }
        if let Some(b) = self.bottombar.borrow().as_ref() {
            b.relocalize();
        }
        if let Some(p) = self.symbol.borrow().as_ref() {
            p.relabel();
        }
        if let Some(p) = self.indicators.borrow().as_ref() {
            p.relabel();
        }
        if let Some(q) = self.quick.borrow().as_ref() {
            q.relabel();
        }
        // The sheets are built on open: dropped here so the next open reads the language.
        if let Some(d) = self.tf_drawer.borrow_mut().take() {
            d.destroy();
        }
        if let Some(d) = self.tz_drawer.borrow_mut().take() {
            d.destroy();
        }
        let overlays: Vec<Rc<CellOverlay>> = self.overlays.borrow().values().cloned().collect();
        for o in overlays {
            o.relocalize();
        }
        self.project_active();
        self.project_alerts();
    }

    /// The chrome size class flipped: a desktop card must not linger over the mobile chrome.
    fn on_layout_mode(&self, mode: LayoutMode) {
        if let Some(p) = self.symbol.borrow().as_ref() {
            p.close();
        }
        if let Some(p) = self.indicators.borrow().as_ref() {
            p.close();
        }
        if let Some(q) = self.quick.borrow().as_ref() {
            q.close();
        }
        if let Some(d) = self.tf_drawer.borrow().as_ref() {
            d.close();
        }
        if let Some(d) = self.tz_drawer.borrow().as_ref() {
            d.close();
        }
        // The alerts dropdown goes too (the original destroys it on the flip).
        let alerts = self.alerts_menu.borrow_mut().take();
        if let Some(menu) = alerts {
            menu.close();
        }
        self.close_chart_dialogs();
        let overlays: Vec<Rc<CellOverlay>> = self.overlays.borrow().values().cloned().collect();
        for o in overlays {
            o.set_mobile(mode == LayoutMode::Mobile);
        }
    }

    // ── topbar items ──

    fn item_visible(&self, id: TopbarItemId) -> bool {
        topbar_has(&self.composition(), id.as_str()) && !self.hidden.borrow().contains(id.as_str())
    }

    fn set_item_visible(&self, id: TopbarItemId, visible: bool) {
        let name = id.as_str();
        if visible == !self.hidden.borrow().contains(name) {
            return;
        }
        if visible {
            self.hidden.borrow_mut().remove(name);
        } else {
            self.hidden.borrow_mut().insert(name);
        }
        if let Some(t) = self.topbar.borrow().as_ref() {
            t.set_item_visible(name, visible);
        }
        if id == TopbarItemId::Alerts && !visible {
            // A hidden bell leaves no open dropdown behind.
            let alerts = self.alerts_menu.borrow_mut().take();
            if let Some(menu) = alerts {
                menu.close();
            }
        }
        if id == TopbarItemId::Screenshot {
            if visible {
                if let Some(me) = self.me.upgrade() {
                    me.register_screenshot_key();
                }
            } else if let Some(off) = self.screenshot_key.borrow_mut().take() {
                off();
            }
        }
    }

    // ── teardown ──

    fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        let root = self.env.root();
        let ws = self.ws();
        if let Some(ws) = &ws {
            for sub in self.subs.borrow_mut().drain(..) {
                ws.off(sub);
            }
        }
        for u in self.unsubs.borrow_mut().drain(..) {
            u();
        }
        self.listeners.borrow_mut().clear();
        if let Some(off) = self.screenshot_key.borrow_mut().take() {
            off();
        }
        if let Some(menu) = self.alerts_menu.borrow_mut().take() {
            menu.close();
        }
        let overlays: Vec<Rc<CellOverlay>> =
            self.overlays.borrow_mut().drain().map(|(_, o)| o).collect();
        for o in overlays {
            o.destroy();
        }
        // Open dialogs release their scope pushes before the keymap goes.
        let open = self.dialogs.replace(0);
        if let Some(root) = &root {
            for _ in 0..open {
                root.keymap().pop_scope("dialog");
            }
        }
        if let Some(p) = self.symbol.borrow_mut().take() {
            p.destroy();
        }
        if let Some(p) = self.indicators.borrow_mut().take() {
            p.destroy();
        }
        if let Some(q) = self.quick.borrow_mut().take() {
            q.destroy();
        }
        if let Some(d) = self.tf_drawer.borrow_mut().take() {
            d.destroy();
        }
        if let Some(d) = self.tz_drawer.borrow_mut().take() {
            d.destroy();
        }
        if let Some(t) = self.topbar.borrow_mut().take() {
            t.destroy();
        }
        if let Some(b) = self.bottombar.borrow_mut().take() {
            b.destroy();
        }
        self.mounts.borrow_mut().clear();
        let _ = attr;
    }
}
