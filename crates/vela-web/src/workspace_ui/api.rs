//! The JavaScript API of the workspace: [`VelaWorkspace`], [`VelaWorkspaceCell`] and
//! [`VelaWidget`].
//!
//! ```js
//! const ws = new VelaWorkspace(el, {
//!   layout: '4',                          // a registered id; false: one chart (VelaWidget)
//!   cells: { btc: { symbol: 'BTC', timeframe: '60' }, eth: { symbol: 'ETH' } },
//!   bars: { BTC: { 60: [bar, ...] }, ETH: [bar, ...] },   // seeds the in-memory provider
//!   sync: { symbol: true, crosshair: true },
//!   theme: 'dark', locale: 'tr', timezone: 'Europe/Istanbul',
//!   persist: 'my-key',                    // true: 'vela-workspace'; localStorage
//! });
//! await ws.ready();
//! ws.cell('btc').setSymbol('ETH');
//! ws.tick('BTC', '60', { time, open, high, low, close, volume });
//! ```
//!
//! # Options
//!
//! | Key | Meaning |
//! |-----|---------|
//! | `layout` | layout id (default `'4'`); `false`: single-chart mode (the widget) |
//! | `cells` | per-cell seeds keyed by a free-form cell name (declaration order fills the slots), or an array of `{ id, ... }`; a seed: `symbol`, `timeframe`, `priceStyle`, `session`, `bars` (depth), `data` (offline bars) |
//! | `bars` | `{ SYMBOL: [bars] }` (any timeframe) or `{ SYMBOL: { timeframe: [bars] } }`: the in-memory provider's series |
//! | `symbols` | `{ SYMBOL: { description, type, prefix } }`: what the symbol search lists for a symbol of `bars` (`prefix` is its listing venue) |
//! | `symbol`, `timeframe` | the default of every cell |
//! | `providers` | real providers to register next to the memory one: `'binance'`, `'coinbase'`, `'hyperliquid'` (they reach their exchange endpoints) |
//! | `sync` | `{ symbol, timeframe, viewport, crosshair, drawings, style }`, each `true` or a group map |
//! | `theme` | `'dark'`, `'light'` or the id of a theme added with `registerTheme` |
//! | `locale`, `timezone` | the workspace language and display zone |
//! | `direction`, `numberingSystem`, `hourCycle`, `weekStart` | the rest of the localization: `'ltr' / 'rtl' / 'auto'` (`dir` is stamped on the root only when given), a CLDR numbering system, `'h23' / 'h12' / 'locale'`, 0 / 1 / 6 |
//! | `backend` | `'auto'` (default), `'webgl2'` or `'canvas2d'`: the geometry backend of every cell (`nativeBackend` is accepted as an alias); an explicit backend wins over the budget |
//! | `maxWebglCells` | above this many cells EVERY cell uses canvas2d (default 8); a layout change that crosses it rebuilds the cells |
//! | `glow` | the WebGL2 neon-glow amount of every cell (default 0) |
//! | `persist` | `true` (the shell's key), a key, or `false` |
//! | `live`, `volume`, `statusline`, `watermark` | booleans, defaults true |
//! | `layoutMode` | `'auto'` (default), `'mobile'`, `'desktop'` |
//! | `autofocus` | focus the active chart on mount (default false) |
//! | `topbar` | `{ left: [ids], right: [ids] }`: the visible entries per side, in render order (the original's `topbar` composition; an undeclared side keeps its default) |
//! | `timeframes` | the topbar's timeframe presets, e.g. `['1', '5', '60', 'D']` |
//! | `indicatorPicker` | `false` removes the built-in indicator picker's entry points |
//! | `bottombar` | `false` removes the bottom bar (default true) |
//! | `sidebar` | `true`, `false` or `{ top: [ids], bottom: [ids] }`: the right-edge toolbar (default off, unless `registerShellDefaults({ sidebar })` says otherwise) |
//!
//! Defaults, as in the original: the topbar, the bottom bar, the status line and the
//! watermark show; the sidebar does not; a widget (`layout: false`) has no layout button.
//! The language packs register lazily (`locale`, `setLocale`; see `crate::locales`).
//!
//! A bar is `{ time, open, high, low, close, volume }` (epoch ms).
//!
//! The container is an element or a CSS selector string.
//!
//! # Contributions
//!
//! The page-wide `register*` functions (`registerWidgetAction`, `registerSidePanel`,
//! `registerWidgetAttachment`, `registerLegendAction`, `registerLegendCallout`,
//! `registerSymbolRanking`, `registerStatePersistence`, `registerShellDefaults`,
//! `registerLocale`) are in [`super::registry`] and `crate::locales`; `refreshActions()` on
//! the workspace and the widget re-projects after a late registration.
//!
//! # Events
//!
//! `on(name, fn)` returns a function that removes the handler; `off(name, fn)` removes by
//! identity. Names and payloads: `cell:active` `{ id, prev }`, `layout:changed`
//! `{ layout }`, `cell:maximized` `{ id }`, `cell:created` / `cell:destroyed` `{ id }`,
//! `state:changed`, `locale:changed` `{ locale, direction }`, `sync:changed`,
//! `timezone:changed`, `alerts:changed`, `projection` `{ id }`, `cell:priceStyle`
//! `{ id, from, to }`, `cell:statusPrefs` `{ id }` (a cell's status-line / watermark
//! prefs changed, active or not), `panels:restored` (`applyState` restored the dock).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Array, Function, Object, Promise, Reflect};
use vela_core::data::providers::binance::BinanceProvider;
use vela_core::data::providers::coinbase::CoinbaseProvider;
use vela_core::data::providers::hyperliquid::HyperliquidProvider;
use vela_core::engine::orchestrator::NativeIndicatorOptions;
use vela_core::i18n::{LocaleRegistry, LocalizationOptions};
use vela_core::options::{ThemeSpec, VelaOptions};
use vela_core::ports::data_provider::{DataProvider, SymbolDescriptor};
use vela_core::ports::market_data::MarketConfig;
use vela_core::runtime::Platform;
use vela_core::state::{SyncKind, SyncOptions, SyncSetting, encode_state};
use vela_core::vela::{Vela, VelaEnv};
use vela_core::widget::contributions::{StatePersistenceHandler, StateScope};
use vela_core::widget::layout_mode::LayoutModeOption;
use vela_core::widget::shell_options::VelaShellOptions;
use vela_core::widget::topbar_composition::TopbarComposition;
use vela_core::workspace::{
    CellExtHandler, CellSeed, ChartCell, GlobalExtHandler, LayoutChoice, ToastKind, Workspace,
    WorkspaceEvent, WorkspaceEventName, WorkspaceHost, WorkspaceOptions, WorkspaceStorage,
    WorkspaceSubscription,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use web_sys::Element;

use crate::app::{bars_of, inputs_of};
use crate::chart::theme_named;
use crate::drawings_api;
use crate::intl::BrowserIntl;
use crate::platform::BrowserPlatform;

use super::host::BrowserWorkspaceHost;
use super::js;
use super::memory_provider::{MemoryProvider, SymbolSeries};
use super::registry;
use super::root::{Region, RootOptions, WidgetRoot};
use super::storage::LocalStorageAdapter;

fn err(message: impl Into<String>) -> JsValue {
    JsError::new(&message.into()).into()
}

/// The container: an element, or a CSS selector string as the original accepts.
fn container_of(value: &JsValue) -> Result<Element, JsValue> {
    if let Some(selector) = value.as_string() {
        return crate::dom::global_document()
            .and_then(|d| d.query_selector(&selector).ok().flatten())
            .ok_or_else(|| err(format!("vela: container not found: {selector}")));
    }
    if value.is_object() {
        return Ok(value.clone().unchecked_into());
    }
    Err(err("vela: the container must be an element or a selector"))
}

// ── option reading ──

fn prop(o: &JsValue, key: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn present(v: &JsValue) -> bool {
    !v.is_undefined() && !v.is_null()
}

fn opt_str(o: &JsValue, key: &str) -> Option<String> {
    prop(o, key).as_string()
}

fn opt_bool(o: &JsValue, key: &str) -> Option<bool> {
    prop(o, key).as_bool()
}

fn entries(o: &JsValue) -> Vec<(String, JsValue)> {
    if !o.is_object() {
        return Vec::new();
    }
    Object::entries(o.unchecked_ref())
        .iter()
        .filter_map(|e| {
            let pair: Array = e.unchecked_into();
            Some((pair.get(0).as_string()?, pair.get(1)))
        })
        .collect()
}

fn seed_of(o: &JsValue) -> Result<CellSeed, JsValue> {
    let mut seed = CellSeed {
        symbol: opt_str(o, "symbol"),
        timeframe: opt_str(o, "timeframe"),
        price_style: opt_str(o, "priceStyle"),
        session: opt_str(o, "session"),
        bars: prop(o, "bars")
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| n as usize),
        ..CellSeed::default()
    };
    let data = prop(o, "data");
    if let Some(list) = data.dyn_ref::<Array>() {
        seed.data = Some(bars_of(list)?);
    }
    Ok(seed)
}

/// `cells`: an object keyed by cell name, or an array of `{ id, ... }`.
fn cells_of(value: &JsValue) -> Result<Vec<(String, CellSeed)>, JsValue> {
    let mut out = Vec::new();
    if let Some(list) = value.dyn_ref::<Array>() {
        for item in list.iter() {
            let id = opt_str(&item, "id")
                .or_else(|| opt_str(&item, "name"))
                .ok_or_else(|| err("vela: each entry of `cells` needs an `id`"))?;
            out.push((id, seed_of(&item)?));
        }
    } else {
        for (name, item) in entries(value) {
            out.push((name, seed_of(&item)?));
        }
    }
    Ok(out)
}

fn sync_kind(name: &str) -> Result<SyncKind, JsValue> {
    SyncKind::ALL
        .into_iter()
        .find(|k| k.as_str() == name)
        .ok_or_else(|| err(format!("vela: unknown sync link \"{name}\"")))
}

fn sync_setting(value: &JsValue) -> Option<SyncSetting> {
    if let Some(b) = value.as_bool() {
        return Some(SyncSetting::Flag(b));
    }
    if value.is_object() {
        let groups: BTreeMap<String, String> = entries(value)
            .into_iter()
            .filter_map(|(k, v)| Some((k, v.as_string()?)))
            .collect();
        return Some(SyncSetting::Groups(groups));
    }
    None
}

fn sync_of(value: &JsValue) -> Result<SyncOptions, JsValue> {
    let mut out = SyncOptions::default();
    for (name, v) in entries(value) {
        out.set(sync_kind(&name)?, sync_setting(&v));
    }
    Ok(out)
}

fn series_of(value: &JsValue) -> Result<SymbolSeries, JsValue> {
    if let Some(list) = value.dyn_ref::<Array>() {
        return Ok(SymbolSeries::new().with_any(bars_of(list)?));
    }
    let mut series = SymbolSeries::new();
    for (timeframe, bars) in entries(value) {
        let list = bars
            .dyn_ref::<Array>()
            .ok_or_else(|| err(format!("vela: bars for \"{timeframe}\" must be an array")))?;
        series = series.with_timeframe(&timeframe, bars_of(list)?);
    }
    Ok(series)
}

fn string_list(value: &JsValue) -> Option<Vec<String>> {
    let list = value.dyn_ref::<Array>()?;
    Some(list.iter().filter_map(|x| x.as_string()).collect())
}

/// `topbar: { left: [ids], right: [ids] }`: the visible entries per side, in render order.
fn topbar_of(o: &JsValue) -> Option<TopbarComposition> {
    let value = prop(o, "topbar");
    if !value.is_object() {
        return None;
    }
    Some(TopbarComposition {
        left: string_list(&prop(&value, "left")),
        right: string_list(&prop(&value, "right")),
    })
}

/// `(backend, glow, max_webgl_cells)`, each absent unless the host gave it.
type RenderOptions = (
    Option<vela_core::options::NativeBackend>,
    Option<f64>,
    Option<usize>,
);

/// The renderer options of the widget: `backend` (`"auto" | "webgl2" | "canvas2d"`, the
/// original's `nativeBackend`), `glow` (the WebGL2 neon amount) and `maxWebglCells` (above
/// this many cells every cell falls back to canvas2d; an explicit backend wins).
fn render_options(o: &JsValue) -> Result<RenderOptions, JsValue> {
    let mut backend = None;
    for key in ["backend", "nativeBackend"] {
        let v = prop(o, key);
        if !present(&v) {
            continue;
        }
        let name = v
            .as_string()
            .ok_or_else(|| err(format!("vela: options.{key} must be a string")))?;
        backend = Some(crate::gl::parse_backend(&name)?);
        break;
    }
    let glow = match prop(o, "glow") {
        v if !present(&v) => None,
        v => Some(
            v.as_f64()
                .filter(|g| g.is_finite() && *g >= 0.0)
                .ok_or_else(|| err("vela: options.glow must be a non-negative number"))?,
        ),
    };
    let max = match prop(o, "maxWebglCells") {
        v if !present(&v) => None,
        v => Some(
            v.as_f64()
                .filter(|m| m.is_finite() && *m >= 0.0)
                .ok_or_else(|| err("vela: options.maxWebglCells must be a non-negative number"))?
                as usize,
        ),
    };
    Ok((backend, glow, max))
}

/// The localization fields of a JS options object (or a `setLocale` object): `locale`,
/// `direction` (`"ltr" | "rtl" | "auto"`), `numberingSystem`, `hourCycle` (`"h23" | "h12" |
/// `"locale"`) and `weekStart` (0, 1 or 6).
fn localization_of(o: &JsValue) -> Result<LocalizationOptions, JsValue> {
    use vela_core::format::types::HourCycleSetting;
    use vela_core::i18n::DirectionOption;
    let direction = match opt_str(o, "direction").as_deref() {
        None => None,
        Some("ltr") => Some(DirectionOption::Ltr),
        Some("rtl") => Some(DirectionOption::Rtl),
        Some("auto") => Some(DirectionOption::Auto),
        Some(other) => {
            return Err(err(format!(
                "vela: unknown direction \"{other}\" (use \"ltr\", \"rtl\" or \"auto\")"
            )));
        }
    };
    let hour_cycle = match opt_str(o, "hourCycle").as_deref() {
        None => None,
        Some("h23") => Some(HourCycleSetting::H23),
        Some("h12") => Some(HourCycleSetting::H12),
        Some("locale") => Some(HourCycleSetting::Locale),
        Some(other) => {
            return Err(err(format!(
                "vela: unknown hourCycle \"{other}\" (use \"h23\", \"h12\" or \"locale\")"
            )));
        }
    };
    let week_start = prop(o, "weekStart")
        .as_f64()
        .filter(|d| [0.0, 1.0, 6.0].contains(d))
        .map(|d| d as u8);
    Ok(LocalizationOptions {
        locale: opt_str(o, "locale"),
        direction,
        numbering_system: opt_str(o, "numberingSystem"),
        hour_cycle,
        week_start,
        ..LocalizationOptions::default()
    })
}

fn layout_mode_of(value: Option<String>) -> Result<LayoutModeOption, JsValue> {
    match value.as_deref() {
        None | Some("auto") => Ok(LayoutModeOption::Auto),
        Some("mobile") => Ok(LayoutModeOption::Mobile),
        Some("desktop") => Ok(LayoutModeOption::Desktop),
        Some(other) => Err(err(format!(
            "vela: unknown layoutMode \"{other}\" (use \"auto\", \"mobile\" or \"desktop\")"
        ))),
    }
}

fn real_provider(name: &str, platform: &Rc<dyn Platform>) -> Result<Rc<dyn DataProvider>, JsValue> {
    Ok(match name {
        "binance" => Rc::new(BinanceProvider::new(platform.clone())),
        "coinbase" => Rc::new(CoinbaseProvider::new(platform.clone())),
        "hyperliquid" => Rc::new(HyperliquidProvider::new(platform.clone())),
        other => {
            return Err(err(format!(
                "vela: unknown provider \"{other}\" (use \"binance\", \"coinbase\" or \"hyperliquid\")"
            )));
        }
    })
}

// ── events ──

fn event_name(name: &str) -> Option<WorkspaceEventName> {
    use WorkspaceEventName as N;
    Some(match name {
        "cell:active" => N::CellActive,
        "layout:changed" => N::LayoutChanged,
        "cell:maximized" => N::CellMaximized,
        "cell:created" => N::CellCreated,
        "cell:destroyed" => N::CellDestroyed,
        "state:changed" => N::StateChanged,
        "locale:changed" => N::LocaleChanged,
        "projection" => N::Projection,
        "sync:changed" => N::SyncChanged,
        "timezone:changed" => N::TimezoneChanged,
        "alerts:changed" => N::AlertsChanged,
        "cell:priceStyle" => N::CellPriceStyle,
        "cell:statusPrefs" => N::StatusPrefs,
        "panels:restored" => N::PanelsRestored,
        _ => return None,
    })
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in fields {
        let _ = Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o.into()
}

fn opt_js(s: &Option<String>) -> JsValue {
    s.as_deref().map_or(JsValue::NULL, JsValue::from_str)
}

fn payload(ev: &WorkspaceEvent) -> JsValue {
    match ev {
        WorkspaceEvent::CellActive { id, prev } => {
            object(&[("id", JsValue::from_str(id)), ("prev", opt_js(prev))])
        }
        WorkspaceEvent::LayoutChanged { layout } => {
            object(&[("layout", JsValue::from_str(layout))])
        }
        WorkspaceEvent::CellMaximized { id } => object(&[("id", opt_js(id))]),
        WorkspaceEvent::CellCreated { id }
        | WorkspaceEvent::CellDestroyed { id }
        | WorkspaceEvent::StatusPrefs { id }
        | WorkspaceEvent::Projection { id } => object(&[("id", JsValue::from_str(id))]),
        WorkspaceEvent::CellPriceStyle { id, from, to } => object(&[
            ("id", JsValue::from_str(id)),
            ("from", JsValue::from_str(from)),
            ("to", JsValue::from_str(to)),
        ]),
        WorkspaceEvent::LocaleChanged(change) => object(&[
            ("locale", JsValue::from_str(&change.locale)),
            (
                "direction",
                JsValue::from_str(match change.direction {
                    vela_core::intl::TextDirection::Rtl => "rtl",
                    vela_core::intl::TextDirection::Ltr => "ltr",
                }),
            ),
        ]),
        _ => JsValue::UNDEFINED,
    }
}

struct Handler {
    event: String,
    callback: Function,
    sub: WorkspaceSubscription,
}

// ── the shared core of the workspace and the widget ──

struct Core {
    ws: Workspace,
    host: Rc<BrowserWorkspaceHost>,
    root: Rc<WidgetRoot>,
    memory: Rc<MemoryProvider>,
    locales: Arc<LocaleRegistry>,
    handlers: RefCell<Vec<Handler>>,
    destroyed: Cell<bool>,
}

impl Core {
    fn live(&self) -> Result<(), JsValue> {
        if self.destroyed.get() {
            Err(err("vela: the workspace was destroyed"))
        } else {
            Ok(())
        }
    }

    fn chart(&self, id: &str) -> Result<Vela, JsValue> {
        self.live()?;
        self.ws
            .cell(id)
            .and_then(|c| c.try_chart())
            .ok_or_else(|| err(format!("vela: no cell \"{id}\"")))
    }

    fn active_id(&self) -> Result<String, JsValue> {
        self.live()?;
        self.ws
            .active_id()
            .ok_or_else(|| err("vela: the workspace has no active cell"))
    }

    fn build(container: &Element, options: &JsValue, single: bool) -> Result<Rc<Core>, JsValue> {
        crate::install();
        let o = if options.is_object() {
            options.clone()
        } else {
            Object::new().into()
        };
        let platform: Rc<dyn Platform> = Rc::new(BrowserPlatform::new());
        let mut env = VelaEnv::with_intl(platform.clone(), Arc::new(BrowserIntl));
        crate::themes::attach(&mut env);
        // The shipped packs register lazily, when a locale asks for one.
        let locales = env.locales.clone();
        crate::locales::attach(&locales);

        // theme
        let theme_name = opt_str(&o, "theme").unwrap_or_else(|| "dark".to_string());
        theme_named(&theme_name)?;
        let theme = ThemeSpec::from(theme_name.as_str());

        // data: the in-memory provider, then the real ones asked for
        let memory = MemoryProvider::new("memory");
        let mut first_symbol: Option<(String, Option<String>)> = None;
        let bars = prop(&o, "bars");
        for (symbol, value) in entries(&bars) {
            let series = series_of(&value)?;
            if first_symbol.is_none() {
                let tf = if value.is_object() && !Array::is_array(&value) {
                    entries(&value).first().map(|(k, _)| k.clone())
                } else {
                    None
                };
                first_symbol = Some((symbol.clone(), tf));
            }
            memory.set_symbol(&symbol, series);
        }
        // What the symbol list says about each symbol: `symbols: { AAA: { description, type, prefix } }`.
        for (symbol, info) in entries(&prop(&o, "symbols")) {
            memory.set_info(
                &symbol,
                SymbolDescriptor {
                    description: opt_str(&info, "description"),
                    kind: opt_str(&info, "type"),
                    prefix: opt_str(&info, "prefix"),
                    ..SymbolDescriptor::default()
                },
            );
        }
        let mut providers: Vec<(String, Rc<dyn DataProvider>)> =
            vec![("memory".to_string(), memory.clone() as Rc<dyn DataProvider>)];
        let wanted = prop(&o, "providers");
        if let Some(list) = wanted.dyn_ref::<Array>() {
            for name in list.iter() {
                let name = name
                    .as_string()
                    .ok_or_else(|| err("vela: `providers` lists provider names"))?;
                if !providers.iter().any(|(n, _)| *n == name) {
                    providers.push((name.clone(), real_provider(&name, &platform)?));
                }
            }
        }

        // cells and defaults
        let timeframe = opt_str(&o, "timeframe")
            .or_else(|| first_symbol.as_ref().and_then(|(_, tf)| tf.clone()))
            .unwrap_or_else(|| "60".to_string());
        let default_symbol = opt_str(&o, "symbol").or_else(|| first_symbol.map(|(s, _)| s));
        let cells_value = prop(&o, "cells");
        let mut cells = if present(&cells_value) {
            cells_of(&cells_value)?
        } else {
            Vec::new()
        };
        if cells.is_empty() && !single {
            // No `cells`: one cell per seeded symbol, named after it.
            for ticker in memory.tickers() {
                cells.push((
                    ticker.to_lowercase(),
                    CellSeed {
                        symbol: Some(ticker),
                        ..CellSeed::default()
                    },
                ));
            }
        }

        let localization = localization_of(&o)?;
        let locale = localization.locale.clone();
        let direction_explicit = localization.direction.is_some();
        if let Some(tag) = &locale {
            crate::locales::ensure(&locales, tag);
        }
        let layout = {
            let l = prop(&o, "layout");
            if single || l.as_bool() == Some(false) {
                LayoutChoice::Single
            } else {
                LayoutChoice::Id(l.as_string().unwrap_or_else(|| "4".to_string()))
            }
        };
        let persist_key = {
            let p = prop(&o, "persist");
            if let Some(key) = p.as_string() {
                Some(key)
            } else if p.as_bool() == Some(true) {
                Some(
                    if single {
                        "vela-widget"
                    } else {
                        "vela-workspace"
                    }
                    .to_string(),
                )
            } else {
                None
            }
        };
        let layout_mode = layout_mode_of(opt_str(&o, "layoutMode"))?;
        let sync = {
            let s = prop(&o, "sync");
            if present(&s) {
                sync_of(&s)?
            } else {
                SyncOptions::default()
            }
        };
        let (native_backend, glow, max_webgl_cells) = render_options(&o)?;
        let storage = Rc::new(LocalStorageAdapter::new());
        let ws_opts = WorkspaceOptions {
            chart: VelaOptions {
                market: MarketConfig {
                    symbol: default_symbol,
                    timeframe: Some(timeframe),
                    ..MarketConfig::default()
                },
                localization: localization.clone(),
                live: Some(opt_bool(&o, "live").unwrap_or(true)),
                volume: Some(opt_bool(&o, "volume").unwrap_or(true)),
                theme: Some(theme.clone()),
                native_backend,
                glow,
                ..VelaOptions::default()
            },
            layout,
            cells,
            sync,
            timezone: opt_str(&o, "timezone"),
            persist: persist_key,
            storage: Some(storage as Rc<dyn WorkspaceStorage>),
            providers,
            statusline: opt_bool(&o, "statusline").unwrap_or(true),
            watermark: opt_bool(&o, "watermark").unwrap_or(true),
            cell_ext_handlers: cell_ext_handlers(),
            max_webgl_cells: max_webgl_cells.unwrap_or(WorkspaceOptions::default().max_webgl_cells),
            ..WorkspaceOptions::default()
        };

        // The host's own value wins over a plugin's default (`registerShellDefaults`).
        let defaults = registry::global_shell_defaults().defaults();
        let shell = VelaShellOptions {
            layout_mode: Some(layout_mode),
            autofocus: opt_bool(&o, "autofocus"),
            statusline: opt_bool(&o, "statusline"),
            watermark: opt_bool(&o, "watermark"),
            bottombar: opt_bool(&o, "bottombar"),
            sidebar: crate::shell_side::sidebar_option_of(&o).or(defaults.sidebar),
            topbar: topbar_of(&o),
            timeframes: string_list(&prop(&o, "timeframes")),
            indicator_picker: opt_bool(&o, "indicatorPicker"),
            timezone: opt_str(&o, "timezone"),
            ..VelaShellOptions::default()
        };
        let root = WidgetRoot::new(
            container,
            platform,
            env.clone(),
            RootOptions {
                theme,
                layout_mode,
                shell,
            },
        )?;
        let host = BrowserWorkspaceHost::new(root.clone(), env.registries.chart_types.clone());
        let mut ws_opts = ws_opts;
        ws_opts.global_ext_handlers = global_ext_handlers(&root);
        let ws = match Workspace::new(host.clone() as Rc<dyn WorkspaceHost>, env, ws_opts) {
            Ok(ws) => ws,
            Err(e) => {
                host.dispose();
                root.destroy();
                return Err(err(e.to_string()));
            }
        };
        host.attach(&ws);
        // The side of the shell: sidebar, side panels, context menu, keys, mobile surfaces.
        // The root owns it from here (it is destroyed with the root).
        let side = crate::shell_side::mount(&root);
        // The top and the bottom of the shell and the per-cell overlays (removed with the root).
        // The dialogs of the top tell the side's typing router when they are up.
        let counter = |f: fn(&crate::shell_side::ShellSide, bool)| -> Option<Rc<dyn Fn(bool)>> {
            let side = side.clone();
            Some(Rc::new(move |open| f(&side, open)))
        };
        let top = Rc::new(crate::shell_top::mount_with(
            &root,
            crate::shell_top::TopHooks {
                single_chart: single,
                on_dialog: counter(|s, open| s.count_dialog(open)),
                on_symbol_search: counter(|s, open| s.set_symbol_search_open(open)),
                on_timeframe_entry: counter(|s, open| s.set_timeframe_entry_open(open)),
                ..Default::default()
            },
        ));
        wire_top_and_side(&top, &side);
        // The contributions: attachments mount, the legends of every cell bind.
        root.refresh_actions();
        {
            let top = top.clone();
            root.on_dispose(move || drop(top));
        }
        if let Some(tag) = &locale {
            let dir = match ws.i18n().direction() {
                vela_core::intl::TextDirection::Rtl => "rtl",
                vela_core::intl::TextDirection::Ltr => "ltr",
            };
            root.set_dir_explicit(direction_explicit);
            root.set_locale_attributes(Some(tag), direction_explicit.then_some(dir));
        }
        if opt_bool(&o, "autofocus") == Some(true)
            && let Some(cell) = ws.active()
        {
            cell.focus();
        }
        Ok(Rc::new(Core {
            ws,
            host,
            root,
            memory,
            locales,
            handlers: RefCell::default(),
            destroyed: Cell::new(false),
        }))
    }

    fn on(&self, event: &str, callback: Function) -> Result<JsValue, JsValue> {
        self.live()?;
        let Some(name) = event_name(event) else {
            return Err(err(format!("vela: unknown workspace event \"{event}\"")));
        };
        let cb = callback.clone();
        let sub = self.ws.on(name, move |ev| {
            let _ = cb.call1(&JsValue::NULL, &payload(ev));
        });
        self.handlers.borrow_mut().push(Handler {
            event: event.to_string(),
            callback,
            sub,
        });
        // Weak: this function stays alive in JavaScript until it is called, which a handler
        // that lives as long as the workspace never does. A strong handle would keep the
        // whole destroyed workspace in memory.
        let ws = self.ws.downgrade();
        Ok(Closure::once_into_js(move || {
            if let Some(ws) = ws.upgrade() {
                ws.off(sub);
            }
        }))
    }

    fn off(&self, event: &str, callback: &Function) -> bool {
        let mut handlers = self.handlers.borrow_mut();
        let Some(at) = handlers
            .iter()
            .position(|h| h.event == event && Object::is(&h.callback, callback))
        else {
            return false;
        };
        let h = handlers.remove(at);
        self.ws.off(h.sub)
    }

    fn ready(&self) -> Promise {
        let futures: Vec<_> = self
            .ws
            .cells()
            .into_iter()
            .filter_map(|c| c.try_chart())
            .map(|chart| chart.ready())
            .collect();
        future_to_promise(async move {
            for f in futures {
                f.await.map_err(|e| err(e.to_string()))?;
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The state document. Readable after `destroy` too (as the original's `getState`): a
    /// host reading it while it tears down, or from its own `state:changed` handler during
    /// the flush `destroy` makes, sees what was on screen.
    fn state(&self) -> Result<JsValue, JsValue> {
        js_sys::JSON::parse(&encode_state(&self.ws.get_state()))
    }

    fn apply_state(&self, state: &JsValue) -> Result<(), JsValue> {
        self.live()?;
        let text = js_sys::JSON::stringify(state)
            .map_err(|_| err("vela: applyState takes a state document"))?
            .as_string()
            .ok_or_else(|| err("vela: applyState takes a state document"))?;
        let doc: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| err(format!("vela: applyState: {e}")))?;
        self.ws.apply_state(&doc);
        Ok(())
    }

    /// The layout as one PNG data URL: a grid is composited through the host's surface, one
    /// visible cell paints its own pile on a surface of its size (both in core).
    fn screenshot(&self) -> Option<String> {
        if self.destroyed.get() {
            return None;
        }
        self.ws.screenshot()
    }

    /// `setLocale(tag | { locale?, direction?, numberingSystem?, hourCycle?, weekStart? })`:
    /// the fields present replace their value, the others stay.
    fn set_locale(&self, next: &JsValue) -> Result<(), JsValue> {
        use vela_core::i18n::{LocalizationPatch, Patch};
        self.live()?;
        if let Some(tag) = next.as_string() {
            crate::locales::ensure(&self.locales, &tag);
            self.ws.set_locale(&tag);
            return Ok(());
        }
        if !next.is_object() {
            return Err(err(
                "vela: setLocale takes a locale tag or an options object",
            ));
        }
        let o = localization_of(next)?;
        if let Some(tag) = &o.locale {
            crate::locales::ensure(&self.locales, tag);
        }
        if o.direction.is_some() {
            self.root.set_dir_explicit(true);
        }
        fn set<T>(v: Option<T>) -> Patch<T> {
            v.map_or(Patch::Keep, Patch::Set)
        }
        self.ws.update_localization(LocalizationPatch {
            locale: set(o.locale),
            direction: set(o.direction),
            numbering_system: set(o.numbering_system),
            hour_cycle: set(o.hour_cycle),
            week_start: set(o.week_start),
            ..LocalizationPatch::default()
        });
        Ok(())
    }

    fn refresh_actions(&self) -> Result<(), JsValue> {
        self.live()?;
        self.root.refresh_actions();
        Ok(())
    }

    fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.host.detach(&self.ws);
        self.ws.destroy();
        self.host.dispose();
        self.root.destroy();
        self.handlers.borrow_mut().clear();
    }
}

fn tick_bar(value: &JsValue) -> Result<vela_core::model::Ohlcv, JsValue> {
    let list = Array::of1(value);
    bars_of(&list)?
        .into_iter()
        .next()
        .ok_or_else(|| err("vela: tick takes a bar"))
}

fn region_of(name: &str) -> Result<Region, JsValue> {
    Region::from_name(name).ok_or_else(|| {
        err(format!(
            "vela: unknown region \"{name}\" (use {})",
            Region::ALL.map(|r| r.name()).join(", ")
        ))
    })
}

// ── state persistence handlers (`registerStatePersistence`) ──

struct CellHandler(Rc<StatePersistenceHandler>);

impl CellExtHandler for CellHandler {
    fn key(&self) -> &str {
        self.0.key()
    }

    fn serialize(&self, cell: &ChartCell) -> Option<serde_json::Value> {
        match &*self.0 {
            StatePersistenceHandler::Cell { serialize, .. } => {
                serialize(&registry::cell_state(cell))
            }
            StatePersistenceHandler::Global { .. } => None,
        }
    }

    fn restore(&self, value: &serde_json::Value, cell: &ChartCell) {
        if let StatePersistenceHandler::Cell { restore, .. } = &*self.0 {
            restore(value, &registry::cell_state(cell));
        }
    }
}

struct GlobalHandler {
    handler: Rc<StatePersistenceHandler>,
    root: std::rc::Weak<WidgetRoot>,
}

impl GlobalHandler {
    fn context(&self, ws: &Workspace) -> Option<super::context::RootContext> {
        Some(super::context::RootContext::new(
            &self.root.upgrade()?,
            ws.downgrade(),
        ))
    }
}

impl GlobalExtHandler for GlobalHandler {
    fn key(&self) -> &str {
        self.handler.key()
    }

    fn serialize(&self, ws: &Workspace) -> Option<serde_json::Value> {
        match &*self.handler {
            StatePersistenceHandler::Global { serialize, .. } => serialize(&self.context(ws)?),
            StatePersistenceHandler::Cell { .. } => None,
        }
    }

    fn restore(&self, value: &serde_json::Value, ws: &Workspace) {
        if let StatePersistenceHandler::Global { restore, .. } = &*self.handler
            && let Some(ctx) = self.context(ws)
        {
            restore(value, &ctx);
        }
    }
}

fn cell_ext_handlers() -> Vec<Rc<dyn CellExtHandler>> {
    registry::global_contributions()
        .state_persistence_handlers(StateScope::Cell)
        .into_iter()
        .map(|h| Rc::new(CellHandler(h)) as Rc<dyn CellExtHandler>)
        .collect()
}

fn global_ext_handlers(root: &Rc<WidgetRoot>) -> Vec<Rc<dyn GlobalExtHandler>> {
    registry::global_contributions()
        .state_persistence_handlers(StateScope::Global)
        .into_iter()
        .map(|handler| {
            Rc::new(GlobalHandler {
                handler,
                root: Rc::downgrade(root),
            }) as Rc<dyn GlobalExtHandler>
        })
        .collect()
}

// ── top <-> side ──

/// The mobile menu lists the topbar's contributed actions.
struct TopMobileHost(Rc<dyn Fn() -> Vec<crate::shell_side::MobileAction>>);

impl crate::shell_side::MobileHost for TopMobileHost {
    fn topbar_actions(&self) -> Vec<crate::shell_side::MobileAction> {
        (self.0)()
    }
}

/// What the two halves of the shell owe each other: the dock's panel toggles in the topbar
/// (and their pressed states), and the topbar's contributed actions in the mobile menu.
fn wire_top_and_side(top: &Rc<crate::shell_top::ShellTop>, side: &crate::shell_side::ShellSide) {
    if let Some(dock) = side.dock() {
        let (buttons_top, active_top, dock_weak) =
            (Rc::downgrade(top), Rc::downgrade(top), Rc::downgrade(&dock));
        dock.attach_chrome(
            Rc::new(move |buttons| {
                let Some(t) = buttons_top.upgrade() else {
                    return;
                };
                let dock = dock_weak.clone();
                t.set_panel_buttons(
                    buttons,
                    Rc::new(move |id| {
                        if let Some(d) = dock.upgrade() {
                            d.toggle(id, None);
                        }
                    }),
                );
            }),
            Rc::new(move |id, open| {
                if let Some(t) = active_top.upgrade() {
                    t.set_panel_active(id, open);
                }
            }),
        );
        // The panels docked before the topbar listened.
        dock.publish();
    }
    side.set_mobile_host(Rc::new(TopMobileHost(top.mobile_actions())));
}

// ── VelaWorkspace ──

/// A multi-chart workspace on a container element. See the module documentation.
#[wasm_bindgen]
pub struct VelaWorkspace {
    core: Rc<Core>,
}

#[wasm_bindgen]
impl VelaWorkspace {
    /// Build the workspace in `container` (an element with a size) from `options`.
    #[wasm_bindgen(constructor)]
    pub fn new(container: &JsValue, options: &JsValue) -> Result<VelaWorkspace, JsValue> {
        Ok(VelaWorkspace {
            core: Core::build(&container_of(container)?, options, false)?,
        })
    }

    /// A promise that settles once every cell's first load has finished.
    pub fn ready(&self) -> Promise {
        self.core.ready()
    }

    // ── layout and cells ──

    /// The registered layouts: `[{ id, label, cols, rows, cells }]`.
    pub fn layouts(&self) -> Result<Array, JsValue> {
        self.core.live()?;
        let i18n = self.core.ws.i18n();
        Ok(self
            .core
            .ws
            .layouts()
            .layouts(Some(&*i18n))
            .into_iter()
            .map(|d| {
                object(&[
                    ("id", JsValue::from_str(&d.id)),
                    ("label", JsValue::from_str(&d.label)),
                    ("cols", JsValue::from_f64(d.cols.len() as f64)),
                    ("rows", JsValue::from_f64(d.rows.len() as f64)),
                    ("cells", JsValue::from_f64(d.cells.len() as f64)),
                ])
            })
            .collect())
    }

    /// The current layout id.
    pub fn layout(&self) -> Result<String, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.layout().id)
    }

    /// Switch the grid to a registered layout (or a grid id such as `"g3x2"`). An unknown id
    /// is an error. A single-chart widget ignores it.
    #[wasm_bindgen(js_name = setLayout)]
    pub fn set_layout(&self, id: &str) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.set_layout(id).map_err(|e| err(e.to_string()))
    }

    /// The live cell names in slot order.
    #[wasm_bindgen(js_name = cellIds)]
    pub fn cell_ids(&self) -> Result<Vec<String>, JsValue> {
        self.core.live()?;
        Ok(self
            .core
            .ws
            .cells()
            .iter()
            .map(|c| c.id().to_string())
            .collect())
    }

    /// The active cell's name.
    #[wasm_bindgen(js_name = activeCell)]
    pub fn active_cell(&self) -> Result<Option<String>, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.active_id())
    }

    /// Make a cell the active one.
    #[wasm_bindgen(js_name = setActiveCell)]
    pub fn set_active_cell(&self, id: &str) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.activate(id);
        Ok(())
    }

    /// A handle to one cell, `undefined` for an unknown name.
    pub fn cell(&self, id: &str) -> Result<Option<VelaWorkspaceCell>, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.cell(id).map(|_| VelaWorkspaceCell {
            core: self.core.clone(),
            id: id.to_string(),
        }))
    }

    /// Maximize a cell over the whole grid; `null` restores the layout.
    #[wasm_bindgen(js_name = maximizeCell)]
    pub fn maximize_cell(&self, id: Option<String>) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.maximize_cell(id.as_deref());
        Ok(())
    }

    /// The maximized cell's name, if any.
    #[wasm_bindgen(js_name = maximizedCell)]
    pub fn maximized_cell(&self) -> Result<Option<String>, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.maximized_cell())
    }

    /// Trade the grid slots of two cells.
    #[wasm_bindgen(js_name = swapCells)]
    pub fn swap_cells(&self, a: &str, b: &str) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.swap_cells(a, b);
        Ok(())
    }

    // ── sync ──

    /// One sync link (`symbol`, `timeframe`, `viewport`, `crosshair`, `drawings`, `style`):
    /// with `value` (`true`, `false` or a `{ cell: group }` map) sets it; returns whether
    /// the link is on.
    pub fn sync(&self, kind: &str, value: &JsValue) -> Result<bool, JsValue> {
        self.core.live()?;
        let kind = sync_kind(kind)?;
        if present(value) {
            self.core.ws.sync_set(kind, sync_setting(value));
        }
        Ok(matches!(
            self.core.ws.sync_get(kind),
            Some(SyncSetting::Flag(true) | SyncSetting::Groups(_))
        ))
    }

    // ── state ──

    /// The whole workspace as a plain document (layout, tracks, active cell, sync, per cell
    /// the market, indicators and drawings).
    #[wasm_bindgen(js_name = getState)]
    pub fn get_state(&self) -> Result<JsValue, JsValue> {
        self.core.state()
    }

    /// Restore a document from `getState` (untrusted-safe: malformed fields are dropped).
    #[wasm_bindgen(js_name = applyState)]
    pub fn apply_state(&self, state: &JsValue) -> Result<(), JsValue> {
        self.core.apply_state(state)
    }

    /// Write the persisted document now (the debounce otherwise waits half a second).
    #[wasm_bindgen(js_name = saveNow)]
    pub fn save_now(&self) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.flush_pending_state();
        self.core.ws.persist_now();
        Ok(())
    }

    // ── presentation ──

    /// Switch between `"dark"` and `"light"`.
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &str) -> Result<(), JsValue> {
        self.core.live()?;
        theme_named(theme)?;
        self.core.ws.set_theme(theme);
        self.core.root.refresh_theme();
        Ok(())
    }

    /// The language of the whole workspace: a BCP 47 tag, or `{ locale, direction,
    /// numberingSystem, hourCycle, weekStart }` (the fields present change, the rest stay).
    #[wasm_bindgen(js_name = setLocale)]
    pub fn set_locale(&self, locale: &JsValue) -> Result<(), JsValue> {
        self.core.set_locale(locale)
    }

    /// Re-project the contributions after a late `register*` call: topbar actions, sidebar
    /// entries and panels, the mobile menu, every cell's legend rows, and attachments not
    /// mounted yet.
    #[wasm_bindgen(js_name = refreshActions)]
    pub fn refresh_actions(&self) -> Result<(), JsValue> {
        self.core.refresh_actions()
    }

    /// The display time zone for every cell (an IANA zone, or `"exchange"`).
    #[wasm_bindgen(js_name = setTimezone)]
    pub fn set_timezone(&self, zone: &str) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.set_timezone(zone);
        Ok(())
    }

    pub fn timezone(&self) -> Result<String, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.timezone())
    }

    /// The layout as one PNG data URL: every cell in its slot, or the maximized one alone.
    /// `undefined` while nothing is laid out.
    pub fn screenshot(&self) -> Option<String> {
        self.core.screenshot()
    }

    /// Download the screenshot as a PNG file; whether there was one.
    #[wasm_bindgen(js_name = downloadScreenshot)]
    pub fn download_screenshot(&self, filename: Option<String>) -> bool {
        let Some(url) = self.core.screenshot() else {
            return false;
        };
        let name = filename.unwrap_or_else(|| "vela-workspace.png".to_string());
        js::vela_ws_download(self.core.root.document().as_ref(), &url, &name);
        true
    }

    /// A notice on the grid's status pill (`kind`: `"info"` (default), `"success"`,
    /// `"error"`; `ms` 0 keeps it until replaced).
    pub fn toast(
        &self,
        message: &str,
        kind: Option<String>,
        ms: Option<f64>,
    ) -> Result<(), JsValue> {
        self.core.live()?;
        let kind = match kind.as_deref() {
            None | Some("info") => ToastKind::Info,
            Some("success") => ToastKind::Success,
            Some("error") => ToastKind::Error,
            Some(other) => return Err(err(format!("vela: unknown toast kind \"{other}\""))),
        };
        self.core.ws.toast(message, kind, ms.unwrap_or(3000.0));
        Ok(())
    }

    // ── data ──

    /// Add or replace one symbol's series of the in-memory provider (`bars` as in the
    /// `bars` option: an array, or `{ timeframe: [bars] }`).
    #[wasm_bindgen(js_name = setBars)]
    pub fn set_bars(&self, symbol: &str, bars: &JsValue) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.memory.set_symbol(symbol, series_of(bars)?);
        Ok(())
    }

    /// One live bar for `symbol` at `timeframe` through the in-memory provider (a bar with
    /// the last bar's time replaces it, a newer one opens the next). Returns how many
    /// charts received it.
    pub fn tick(&self, symbol: &str, timeframe: &str, bar: &JsValue) -> Result<usize, JsValue> {
        self.core.live()?;
        Ok(self.core.memory.tick(symbol, timeframe, tick_bar(bar)?))
    }

    // ── events ──

    /// Listen to a workspace event; returns the function that removes the handler.
    pub fn on(&self, event: &str, callback: Function) -> Result<JsValue, JsValue> {
        self.core.on(event, callback)
    }

    /// Remove a handler given to `on`; whether it was registered.
    pub fn off(&self, event: &str, callback: &Function) -> bool {
        self.core.off(event, callback)
    }

    // ── structure ──

    /// The root element (`div.vela-workspace`).
    pub fn root(&self) -> Element {
        self.core.root.element().clone()
    }

    /// The grid element (`div.vela-ws-grid`).
    pub fn grid(&self) -> Element {
        self.core.root.grid().clone()
    }

    /// The names of the root's regions.
    pub fn regions(&self) -> Vec<String> {
        Region::ALL.iter().map(|r| r.name().to_string()).collect()
    }

    /// One region's element (`topbar`, `toolbar`, `main`, `strips`, `bottombar`,
    /// `strips-below`, `mobilebar`, `column`, `edge`).
    pub fn region(&self, name: &str) -> Result<Element, JsValue> {
        Ok(self.core.root.region(region_of(name)?).clone())
    }

    /// The chrome size class, `"desktop"` or `"mobile"`.
    #[wasm_bindgen(js_name = layoutMode)]
    pub fn layout_mode(&self) -> String {
        self.core.root.layout_mode().as_str().to_string()
    }

    /// Tear everything down: the chart cells, listeners, observers, timers and DOM. The
    /// pending state is written first.
    pub fn destroy(&self) {
        self.core.destroy();
    }
}

// ── one cell ──

/// A handle to one cell of a workspace.
#[wasm_bindgen]
pub struct VelaWorkspaceCell {
    core: Rc<Core>,
    id: String,
}

#[wasm_bindgen]
impl VelaWorkspaceCell {
    /// The cell's name.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    fn cell(&self) -> Result<vela_core::workspace::ChartCell, JsValue> {
        self.core.live()?;
        self.core
            .ws
            .cell(&self.id)
            .ok_or_else(|| err(format!("vela: cell \"{}\" is gone", self.id)))
    }

    /// The cell's symbol (as the chart shows it).
    pub fn symbol(&self) -> Result<String, JsValue> {
        Ok(self.cell()?.symbol())
    }

    /// The cell's timeframe.
    pub fn timeframe(&self) -> Result<String, JsValue> {
        Ok(self.cell()?.timeframe())
    }

    /// Switch the cell's symbol (every cell follows while the symbol sync is on).
    #[wasm_bindgen(js_name = setSymbol)]
    pub fn set_symbol(&self, symbol: &str) -> Result<(), JsValue> {
        self.cell()?.set_symbol(symbol);
        Ok(())
    }

    /// Switch the cell's timeframe (`"1"`, `"60"`, `"D"`, `"4h"`, ...).
    #[wasm_bindgen(js_name = setTimeframe)]
    pub fn set_timeframe(&self, timeframe: &str) -> Result<(), JsValue> {
        self.cell()?.set_timeframe(timeframe);
        Ok(())
    }

    /// The cell's price style (`"candles"`, ...).
    #[wasm_bindgen(js_name = priceStyle)]
    pub fn price_style(&self) -> Result<String, JsValue> {
        Ok(self.cell()?.price_style())
    }

    #[wasm_bindgen(js_name = setPriceStyle)]
    pub fn set_price_style(&self, style: &str) -> Result<(), JsValue> {
        self.cell()?.set_price_style(style);
        Ok(())
    }

    /// Switch the shown session in place (`"regular"`, `"extended"`; a reload).
    #[wasm_bindgen(js_name = setSession)]
    pub fn set_session(&self, session: &str) -> Result<(), JsValue> {
        let session = vela_core::ports::market_data::MarketSession::normalize(session)
            .ok_or_else(|| err(format!("vela: unknown session \"{session}\"")))?;
        self.cell()?.set_session(session);
        Ok(())
    }

    /// Show or hide this cell's symbol watermark.
    #[wasm_bindgen(js_name = setWatermarkVisible)]
    pub fn set_watermark_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_watermark_visible(visible);
        Ok(())
    }

    /// Show or hide the indicator titles in this cell's legend.
    #[wasm_bindgen(js_name = setIndicatorTitlesVisible)]
    pub fn set_indicator_titles_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_indicator_titles_visible(visible);
        Ok(())
    }

    /// Show or hide the indicator values in this cell's legend.
    #[wasm_bindgen(js_name = setIndicatorValuesVisible)]
    pub fn set_indicator_values_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_indicator_values_visible(visible);
        Ok(())
    }

    /// Add a built-in native indicator by type (`"sma"`, `"rsi"`, ...), optionally with
    /// `inputs`. Returns the indicator id.
    #[wasm_bindgen(js_name = addIndicator)]
    pub fn add_indicator(&self, kind: &str, inputs: &JsValue) -> Result<String, JsValue> {
        let chart = self.core.chart(&self.id)?;
        let values = inputs_of(inputs);
        let handle = chart.add_native_indicator(
            kind,
            NativeIndicatorOptions {
                inputs: (!values.is_empty()).then_some(values),
            },
        );
        Ok(handle.id().to_string())
    }

    /// The ids of the indicators on the chart.
    pub fn indicators(&self) -> Result<Vec<String>, JsValue> {
        Ok(self
            .core
            .chart(&self.id)?
            .indicators()
            .iter()
            .map(|h| h.id().to_string())
            .collect())
    }

    /// Whether this is the workspace's active cell.
    #[wasm_bindgen(js_name = isActive)]
    pub fn is_active(&self) -> Result<bool, JsValue> {
        self.core.live()?;
        Ok(self.core.ws.active_id().as_deref() == Some(self.id.as_str()))
    }

    /// The geometry backend this cell paints with: `"webgl2"` or `"canvas2d"` (the budget
    /// policy, `backend` and `maxWebglCells` decide; `"canvas2d"` also after a failed context).
    pub fn backend(&self) -> Result<String, JsValue> {
        let renderer = self
            .cell()?
            .renderer()
            .ok_or_else(|| err(format!("vela: cell \"{}\" has no chart yet", self.id)))?;
        Ok(crate::gl::backend_name(renderer.geometry_backend()).to_string())
    }

    /// The cell's container element (`div.vela-cell`).
    pub fn element(&self) -> Option<Element> {
        self.core.root.cell_container(&self.id)
    }

    // ── drawings ──

    /// Arm a drawing tool by type key; `null` returns to the pointer.
    #[wasm_bindgen(js_name = armDrawingTool)]
    pub fn arm_drawing_tool(&self, type_key: Option<String>) -> Result<(), JsValue> {
        drawings_api::arm(self.core.chart(&self.id)?.drawings(), type_key)
    }

    #[wasm_bindgen(js_name = drawingCount)]
    pub fn drawing_count(&self) -> Result<usize, JsValue> {
        Ok(drawings_api::count(self.core.chart(&self.id)?.drawings()))
    }

    /// The cell's drawings as a JSON document (text).
    #[wasm_bindgen(js_name = exportDrawings)]
    pub fn export_drawings(&self) -> Result<String, JsValue> {
        drawings_api::export(self.core.chart(&self.id)?.drawings())
    }

    /// Replace the cell's drawings with a document from `exportDrawings`.
    #[wasm_bindgen(js_name = importDrawings)]
    pub fn import_drawings(&self, json: &str) -> Result<usize, JsValue> {
        drawings_api::import(self.core.chart(&self.id)?.drawings(), json)
    }
}

// ── VelaWidget ──

/// The single-chart widget: the workspace pinned to one cell, in the same root with the
/// same regions (`layout: false`, persisted under `vela-widget` with `persist: true`). Takes
/// the workspace's options (except `layout`).
#[wasm_bindgen]
pub struct VelaWidget {
    core: Rc<Core>,
}

#[wasm_bindgen]
impl VelaWidget {
    #[wasm_bindgen(constructor)]
    pub fn new(container: &JsValue, options: &JsValue) -> Result<VelaWidget, JsValue> {
        Ok(VelaWidget {
            core: Core::build(&container_of(container)?, options, true)?,
        })
    }

    pub fn ready(&self) -> Promise {
        self.core.ready()
    }

    /// The widget's one cell.
    pub fn cell(&self) -> Result<VelaWorkspaceCell, JsValue> {
        let id = self.core.active_id()?;
        Ok(VelaWorkspaceCell {
            core: self.core.clone(),
            id,
        })
    }

    /// The workspace behind the widget (shares its lifetime: `destroy` on either ends both).
    pub fn workspace(&self) -> VelaWorkspace {
        VelaWorkspace {
            core: self.core.clone(),
        }
    }

    pub fn symbol(&self) -> Result<String, JsValue> {
        self.cell()?.symbol()
    }

    pub fn timeframe(&self) -> Result<String, JsValue> {
        self.cell()?.timeframe()
    }

    #[wasm_bindgen(js_name = setSymbol)]
    pub fn set_symbol(&self, symbol: &str) -> Result<(), JsValue> {
        self.cell()?.set_symbol(symbol)
    }

    #[wasm_bindgen(js_name = setTimeframe)]
    pub fn set_timeframe(&self, timeframe: &str) -> Result<(), JsValue> {
        self.cell()?.set_timeframe(timeframe)
    }

    #[wasm_bindgen(js_name = setPriceStyle)]
    pub fn set_price_style(&self, style: &str) -> Result<(), JsValue> {
        self.cell()?.set_price_style(style)
    }

    #[wasm_bindgen(js_name = setSession)]
    pub fn set_session(&self, session: &str) -> Result<(), JsValue> {
        self.cell()?.set_session(session)
    }

    #[wasm_bindgen(js_name = setWatermarkVisible)]
    pub fn set_watermark_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_watermark_visible(visible)
    }

    #[wasm_bindgen(js_name = setIndicatorTitlesVisible)]
    pub fn set_indicator_titles_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_indicator_titles_visible(visible)
    }

    #[wasm_bindgen(js_name = setIndicatorValuesVisible)]
    pub fn set_indicator_values_visible(&self, visible: bool) -> Result<(), JsValue> {
        self.cell()?.set_indicator_values_visible(visible)
    }

    #[wasm_bindgen(js_name = addIndicator)]
    pub fn add_indicator(&self, kind: &str, inputs: &JsValue) -> Result<String, JsValue> {
        self.cell()?.add_indicator(kind, inputs)
    }

    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &str) -> Result<(), JsValue> {
        self.workspace().set_theme(theme)
    }

    #[wasm_bindgen(js_name = setLocale)]
    pub fn set_locale(&self, locale: &JsValue) -> Result<(), JsValue> {
        self.core.set_locale(locale)
    }

    #[wasm_bindgen(js_name = refreshActions)]
    pub fn refresh_actions(&self) -> Result<(), JsValue> {
        self.core.refresh_actions()
    }

    #[wasm_bindgen(js_name = setTimezone)]
    pub fn set_timezone(&self, zone: &str) -> Result<(), JsValue> {
        self.core.live()?;
        self.core.ws.set_timezone(zone);
        Ok(())
    }

    #[wasm_bindgen(js_name = getState)]
    pub fn get_state(&self) -> Result<JsValue, JsValue> {
        self.core.state()
    }

    #[wasm_bindgen(js_name = applyState)]
    pub fn apply_state(&self, state: &JsValue) -> Result<(), JsValue> {
        self.core.apply_state(state)
    }

    pub fn tick(&self, symbol: &str, timeframe: &str, bar: &JsValue) -> Result<usize, JsValue> {
        self.workspace().tick(symbol, timeframe, bar)
    }

    pub fn screenshot(&self) -> Option<String> {
        self.core.screenshot()
    }

    pub fn on(&self, event: &str, callback: Function) -> Result<JsValue, JsValue> {
        self.core.on(event, callback)
    }

    pub fn off(&self, event: &str, callback: &Function) -> bool {
        self.core.off(event, callback)
    }

    pub fn root(&self) -> Element {
        self.core.root.element().clone()
    }

    pub fn regions(&self) -> Vec<String> {
        Region::ALL.iter().map(|r| r.name().to_string()).collect()
    }

    pub fn region(&self, name: &str) -> Result<Element, JsValue> {
        Ok(self.core.root.region(region_of(name)?).clone())
    }

    pub fn destroy(&self) {
        self.core.destroy();
    }
}
