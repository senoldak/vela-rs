//! The public chart (`Vela.ts`): the class a host constructs.
//!
//! [`Vela`] is the composition root. It resolves the options, builds the default
//! market-data feed, registers the built-in chart types and native indicators,
//! creates the [`EngineOrchestrator`] and the control surfaces (`chart.data`,
//! `chart.panes`, `chart.replay`, `chart.renderer`, `chart.drawings`,
//! `chart.marks`), and exposes the chart API.
//!
//! # Differences from the original
//!
//! - **The renderer is required.** The original defaults `options.renderer` to its
//!   `NativeRenderer` (the canvas renderer), which is not ported. Construction
//!   without a renderer (neither [`VelaOptions::renderer`] nor
//!   [`VelaDeps::renderer`]) fails with [`VelaError::MissingRenderer`]. This is the
//!   one place the facade is narrower than the original; the default returns when
//!   the renderer is ported.
//! - **The globals are explicit.** What the original keeps in module globals (the
//!   chart-type registry, the native-indicator registry, the drawing registry, the
//!   theme registry, the locale registry, the renderer defaults, the shared bar
//!   store) is owned by a [`VelaEnv`]. Charts built from one environment share
//!   them, as charts on one page share the globals; [`VelaEnv::new`] registers the
//!   built-ins the original registers at import and construction time.
//! - **No container.** The original's `container` is gone (a DOM-owning renderer
//!   gets its element from its host crate), and with it `options.height`.
//! - **A handle, not an object.** [`Vela`] is a cheap shared handle. Dropping the
//!   last handle destroys the chart (`destroy` is idempotent), which releases the
//!   engine: nothing the facade registers on the engine keeps it alive.
//! - **The translator is replaced, not mutated.** `chart.i18n` is one object for
//!   the chart's lifetime in the original; its Rust translator is immutable, so
//!   [`Vela::set_locale`] builds a new one and hands it to the engine and the
//!   renderer. Read [`Vela::i18n`] again after a locale change.
//! - **`setLocale` is two methods**: [`Vela::set_locale`] for a tag and
//!   [`Vela::update_localization`] for a [`LocalizationPatch`] (the original's
//!   "explicit `undefined` restores the default" is [`Patch::Clear`](crate::i18n::Patch::Clear)).
//! - **`on` returns a [`Subscription`]** (the event bus's own handle), which
//!   [`Vela::off`] takes.
//! - **Errors** are one enum, [`VelaError`]. A script failure stays a value
//!   ([`RunIndicatorResult`], [`ScriptRunResult`]); its message is a `String`.
//! - **Not awaited calls start at once.** `run_indicator` and `run_script` call
//!   `add_indicator` and register their listeners when they are CALLED, not when
//!   the returned future is first polled, as the original's non-`async` methods do.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crate::chart_types::register_builtin_chart_types;
use crate::controls::data_control::DataControl;
use crate::controls::drawings_control::DrawingsControl;
use crate::controls::marks_control::MarksControl;
use crate::controls::panes_control::PanesControl;
use crate::controls::renderer_control::RendererControl;
use crate::controls::replay_control::ReplayControl;
use crate::data::BarStore;
use crate::data::multi_provider_feed::MultiProviderFeed;
use crate::engine::indicator_handle::{IndicatorEvent, IndicatorEventName, IndicatorHandle};
use crate::engine::inspect::SceneInspection;
use crate::engine::orchestrator::{
    EngineOrchestrator, IndicatorIdError, NativeIndicatorOptions, Registries, ResolvedConfig,
    Services,
};
use crate::events::Subscription;
use crate::events::types::{ChartEvent, ChartEventName};
use crate::i18n::{
    LocaleChangedEvent, LocaleRegistry, LocaleState, LocalizationOptions, LocalizationPatch,
    Translator,
};
use crate::indicators::NativeIndicatorInfo;
use crate::indicators::classics::register_classic_indicators;
use crate::intl::{Intl, default_intl};
use crate::options::{
    AddIndicatorOptions, MarketSnapshot, MarketSwitch, NativeBackend, ThemeSpec, VelaOptions,
    resolve_animations,
};
use crate::ports::chart_renderer::{ChartRenderer, RendererRootAttributes, VisibleRange};
use crate::ports::market_data::{MarketDataFeed, SymbolFacts};
use crate::ports::scripting_engine::{EngineContextSnapshot, ScriptingEngine};
use crate::renderer_defaults::RendererDefaults;
use crate::runtime::{DataError, LocalBoxFuture, Platform, Unsubscribe, oneshot};
use crate::script_run::{ScriptRunHandler, ScriptRunResult};
use crate::visible_range::VisibleRangePreset;

use crate::options::RendererDisplayOptions;

/// A live indicator handle as the host sees it.
pub type IndicatorHandleRef = Rc<dyn IndicatorHandle>;

/// Why a [`Vela`] call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum VelaError {
    /// Neither `options.renderer` nor `deps.renderer` was given. The original
    /// falls back to its native renderer, which is not ported yet.
    MissingRenderer,
    /// `add_indicator` refused the id.
    Indicator(IndicatorIdError),
    /// A market load failed (`DataError::Unavailable` is the original's
    /// `DataUnavailableError`).
    Data(DataError),
}

impl fmt::Display for VelaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRenderer => f.write_str(
                "[vela] options.renderer is required: this build has no default renderer. \
                 Pass a renderer factory (or deps.renderer).",
            ),
            Self::Indicator(e) => write!(f, "{e}"),
            Self::Data(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VelaError {}

impl From<IndicatorIdError> for VelaError {
    fn from(e: IndicatorIdError) -> Self {
        Self::Indicator(e)
    }
}

impl From<DataError> for VelaError {
    fn from(e: DataError) -> Self {
        Self::Data(e)
    }
}

/// Outcome of [`Vela::run_indicator`]: success carries the live handle, failure
/// the error.
#[derive(Clone)]
pub struct RunIndicatorResult {
    pub ok: bool,
    /// The mounted indicator on success; `None` on failure (it was removed again).
    pub handle: Option<IndicatorHandleRef>,
    /// The compile/runtime error's message on failure; `None` on success.
    pub error: Option<String>,
    /// Post-mortem context snapshot on failure, when the engine had produced one.
    pub context: Option<EngineContextSnapshot>,
}

impl fmt::Debug for RunIndicatorResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunIndicatorResult")
            .field("ok", &self.ok)
            .field("handle", &self.handle.as_ref().map(|h| h.id().to_string()))
            .field("error", &self.error)
            .field("context", &self.context.is_some())
            .finish()
    }
}

/// Optional dependency overrides: inject a renderer, engines, or a data feed
/// (tests, swaps).
#[derive(Clone, Default)]
pub struct VelaDeps {
    /// A renderer INSTANCE; wins over `options.renderer`.
    pub renderer: Option<Rc<dyn ChartRenderer>>,
    /// Scripting engines to register at construction (bulk form of
    /// `register_engine`); default none.
    pub engines: Vec<Rc<dyn ScriptingEngine>>,
    /// Market-data source; default a [`MultiProviderFeed`] (a provider registry;
    /// offline `data` needs no provider). A custom feed injected here is used
    /// bare: `chart.data` registration is then a no-op.
    pub data_feed: Option<Rc<dyn MarketDataFeed>>,
    /// A [`MultiProviderFeed`] the host built (for instance on its own bar
    /// store) and wants `chart.data` to drive. The original recognizes an
    /// injected `MultiProviderFeed` by `instanceof`; a trait object cannot be
    /// recognized, so the host says so here. Wins over `data_feed`.
    pub registry_feed: Option<MultiProviderFeed>,
}

// ── the environment ────────────────────────────────────────

/// What the original keeps in module globals, owned explicitly: the platform
/// services, the `Intl` backend, the registries, the locale registry, the
/// renderer feature defaults and the shared bar store. Clones share all of it,
/// so charts built from clones of one environment share the globals.
#[derive(Clone)]
pub struct VelaEnv {
    /// Timers, tasks, network, sockets, warnings.
    pub platform: Rc<dyn Platform>,
    /// The `Intl` backend the translators format through.
    pub intl: Arc<dyn Intl>,
    /// Chart types, native indicators, drawing tools, themes.
    pub registries: Registries,
    /// Locale packs (`registerLocale`).
    pub locales: Arc<LocaleRegistry>,
    /// Renderer feature defaults (`registerRendererDefaults`): applied to every
    /// chart built afterwards, once its renderer is mounted.
    pub renderer_defaults: Rc<RefCell<RendererDefaults>>,
    /// The closed-bar cache the default feeds share (`sharedBarStore`).
    pub bar_store: Rc<RefCell<BarStore>>,
}

impl VelaEnv {
    /// An environment on `platform` with the built-in English `Intl` backend and
    /// every built-in registered, in the original's order: the chart types, the
    /// `volume` and `vpvr` native indicators, then the classic indicators.
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self::with_intl(platform, default_intl())
    }

    /// As [`new`](Self::new) with a host `Intl` backend.
    pub fn with_intl(platform: Rc<dyn Platform>, intl: Arc<dyn Intl>) -> Self {
        let env = Self {
            platform,
            locales: LocaleRegistry::new(Arc::clone(&intl)),
            intl,
            registries: Registries::builtin(),
            renderer_defaults: Rc::new(RefCell::new(RendererDefaults::new())),
            bar_store: Rc::new(RefCell::new(BarStore::new())),
        };
        env.register_builtins();
        env
    }

    /// The registrations the original's constructor repeats on every `new Vela`
    /// (each idempotent): built-in chart types, `volume`, `vpvr`, the classics.
    /// A built-in a host replaced or removed comes back, as in the original.
    fn register_builtins(&self) {
        register_builtin_chart_types(&mut self.registries.chart_types.borrow_mut());
        let mut natives = self.registries.native_indicators.borrow_mut();
        crate::indicators::volume::register_volume(&mut natives);
        crate::indicators::vpvr::register_vpvr(&mut natives);
        register_classic_indicators(&mut natives);
    }
}

// ── the chart ──────────────────────────────────────────────

struct VelaInner {
    env: VelaEnv,
    orchestrator: EngineOrchestrator,
    renderer_control: RendererControl,
    panes_control: PanesControl,
    data_control: DataControl,
    drawings_control: DrawingsControl,
    marks_control: MarksControl,
    replay_control: ReplayControl,
    renderer_port: Rc<dyn ChartRenderer>,
    locale: RefCell<Rc<LocaleState>>,
    destroyed: Cell<bool>,
}

impl Drop for VelaInner {
    fn drop(&mut self) {
        // The last handle is gone: stop the engine as `destroy()` would (timers,
        // live feed, renderer), unless the host already did.
        if !self.destroyed.replace(true) {
            self.orchestrator.destroy();
        }
    }
}

/// The public, imperative chart: a cheap shared handle (clones are the same
/// chart). Scripting engines are opt-in: register one with
/// [`register_engine`](Self::register_engine) (no engine means candles only).
#[derive(Clone)]
pub struct Vela {
    inner: Rc<VelaInner>,
}

/// A non-owning reference to a chart; see [`Vela::downgrade`].
#[derive(Clone)]
pub struct WeakVela {
    inner: Weak<VelaInner>,
}

impl WeakVela {
    /// The chart, unless every handle was dropped.
    pub fn upgrade(&self) -> Option<Vela> {
        self.inner.upgrade().map(|inner| Vela { inner })
    }
}

impl Vela {
    /// Build a chart on `env` from `options`. Starts the first load: its
    /// synchronous prefix (`load:start`) has run when this returns.
    pub fn new(env: &VelaEnv, options: VelaOptions) -> Result<Self, VelaError> {
        Self::with_deps(env, options, VelaDeps::default())
    }

    /// As [`new`](Self::new) with dependency overrides.
    pub fn with_deps(
        env: &VelaEnv,
        options: VelaOptions,
        deps: VelaDeps,
    ) -> Result<Self, VelaError> {
        // Built-in chart types, native indicators (idempotent).
        env.register_builtins();
        let platform = Rc::clone(&env.platform);
        let theme = env
            .registries
            .themes
            .borrow()
            .resolve(options.theme.as_ref());
        let theme_color = |key: &str| {
            theme
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let animations = resolve_animations(options.animations.as_ref());
        let display = RendererDisplayOptions {
            current_price_line: options.current_price_line.unwrap_or(true),
            log_scale: options.log_scale.unwrap_or(false),
            native_backend: options.native_backend.unwrap_or(NativeBackend::Auto),
            anim_zoom: animations.anim_zoom,
            anim_pan: animations.anim_pan,
            anim_scroll: animations.anim_scroll,
            anim_autoscale: animations.anim_autoscale,
            anim_live_bar: animations.anim_live_bar,
            anim_intro: animations.anim_intro,
            glow: options.glow.unwrap_or(0.0),
            up_color: options
                .up_color
                .clone()
                .unwrap_or_else(|| theme_color("upColor")),
            down_color: options
                .down_color
                .clone()
                .unwrap_or_else(|| theme_color("downColor")),
            price_style: options
                .price_style
                .clone()
                .unwrap_or_else(|| "candles".to_string()),
            price_axis_ticks: options.price_axis.as_ref().and_then(|p| p.ticks.clone()),
            free_pan: options.free_pan.clone(),
        };
        let renderer: Rc<dyn ChartRenderer> = match (deps.renderer, options.renderer.as_ref()) {
            (Some(renderer), _) => renderer,
            (None, Some(make)) => make(display),
            (None, None) => return Err(VelaError::MissingRenderer),
        };
        let locale = Rc::new(LocaleState::new(
            options.localization.clone(),
            Arc::clone(&env.intl),
            Arc::clone(&env.locales),
        ));
        let engines = deps.engines;
        // Default: a multi-provider registry feed (caches closed bars internally).
        // No provider is bundled: register one with `chart.data().register_provider`;
        // until then a symbol-backed chart parks its initial load. Inject
        // `deps.data_feed` to source candles from your own feed (used bare, no
        // registry).
        let (feed, data_control): (Rc<dyn MarketDataFeed>, DataControl) =
            match (deps.registry_feed, deps.data_feed) {
                (Some(multi), _) => (
                    Rc::new(multi.clone()),
                    DataControl::new(Some(multi), Rc::clone(&platform)),
                ),
                (None, Some(feed)) => (feed, DataControl::new(None, Rc::clone(&platform))),
                (None, None) => {
                    let multi =
                        MultiProviderFeed::new(Rc::clone(&env.bar_store), Rc::clone(&platform));
                    (
                        Rc::new(multi.clone()),
                        DataControl::new(Some(multi), Rc::clone(&platform)),
                    )
                }
            };
        let config = ResolvedConfig {
            market: options.market.clone(),
            live: options.live.unwrap_or(false),
            theme,
            default_language: options
                .default_language
                .clone()
                .or_else(|| engines.first().map(|e| e.language().to_string()))
                .unwrap_or_else(|| "pine".to_string()),
            drawings: options.drawings.clone(),
            volume: Some(options.volume.unwrap_or(true)), // the volume indicator is on by default
        };
        let renderer_control = RendererControl::new(Rc::clone(&renderer), Rc::clone(&platform));
        // The settings-dialog visibility policy is instance state, not chart config:
        // it must never ride `get_config()`/`apply_config()` into exported templates.
        if let Some(settings) = &options.settings {
            renderer_control.set_settings_visibility(settings);
        }
        let orchestrator = EngineOrchestrator::new(
            Rc::clone(&platform),
            Rc::clone(&renderer),
            feed,
            engines,
            config,
            data_control.clone(),
            Services {
                registries: env.registries.clone(),
            },
        );
        // Plugin-contributed renderer defaults (`registerRendererDefaults`), applied
        // once the orchestrator has mounted the renderer and before the first paint.
        // Defaults only: an explicit `renderer.set(...)` or a restored config
        // afterwards still wins.
        let defaults = env.renderer_defaults.borrow().snapshot();
        if !defaults.is_empty() {
            renderer_control.set_many(defaults);
        }
        let panes_control = PanesControl::new(orchestrator.pane_controller(), Rc::clone(&platform));
        let drawings_control = DrawingsControl::new(orchestrator.drawings(), Rc::clone(&platform));
        let marks_control = MarksControl::new(orchestrator.marks());
        let replay_control = ReplayControl::new(orchestrator.replay_controller());
        orchestrator.set_translator(Rc::clone(&locale) as Rc<dyn Translator>);
        let chart = Self {
            inner: Rc::new(VelaInner {
                env: env.clone(),
                orchestrator,
                renderer_control,
                panes_control,
                data_control,
                drawings_control,
                marks_control,
                replay_control,
                renderer_port: renderer,
                locale: RefCell::new(locale),
                destroyed: Cell::new(false),
            }),
        };
        chart.push_localization();
        Ok(chart)
    }

    /// A non-owning reference: it does not keep the chart alive.
    pub fn downgrade(&self) -> WeakVela {
        WeakVela {
            inner: Rc::downgrade(&self.inner),
        }
    }

    /// The environment this chart was built on.
    pub fn env(&self) -> &VelaEnv {
        &self.inner.env
    }

    // ── localization ───────────────────────────────────────

    /// The chart's translator: its resolved locale and direction, `t()` for
    /// messages, and `format()` for numbers and dates in the chart's locale. After
    /// [`set_locale`](Self::set_locale) this returns the NEW translator (the old
    /// object keeps its old settings).
    pub fn i18n(&self) -> Rc<dyn Translator> {
        self.inner.locale.borrow().clone() as Rc<dyn Translator>
    }

    /// Change the chart's locale at runtime (`chart.set_locale("de")`), keeping
    /// every other localization option. See [`update_localization`] for the rest.
    ///
    /// [`update_localization`]: Self::update_localization
    pub fn set_locale(&self, tag: &str) -> &Self {
        self.update_localization(LocalizationPatch::locale(tag))
    }

    /// Change any localization fields at runtime. Fields set in `patch` replace
    /// their current value ([`Patch::Clear`] restores the default); the others are
    /// kept. The chart re-renders its text, axes and readouts in place (no
    /// indicator re-run) and emits `locale:changed`. Register the locale's pack
    /// (`env.locales.register`) first for translated text.
    ///
    /// [`Patch::Clear`]: crate::i18n::Patch::Clear
    pub fn update_localization(&self, patch: LocalizationPatch) -> &Self {
        let next = {
            let old = self.inner.locale.borrow();
            let mut next = LocaleState::new(
                old.options().clone(),
                Arc::clone(old.intl()),
                Arc::clone(old.registry()),
            );
            next.update(patch);
            Rc::new(next)
        };
        *self.inner.locale.borrow_mut() = Rc::clone(&next);
        let orchestrator = &self.inner.orchestrator;
        orchestrator.set_translator(Rc::clone(&next) as Rc<dyn Translator>);
        orchestrator.relocalize();
        self.push_localization();
        orchestrator.events().emit(
            &ChartEventName::LocaleChanged,
            &ChartEvent::LocaleChanged(LocaleChangedEvent {
                locale: next.locale().to_string(),
                direction: next.direction(),
            }),
        );
        self
    }

    /// Hand the renderer the translator and the root attributes the host asked
    /// for: `lang` once a locale is set, `dir` once a direction is (unset, the
    /// host page decides).
    fn push_localization(&self) {
        let state = self.inner.locale.borrow().clone();
        let attributes = RendererRootAttributes {
            lang: state.locale_explicit().then(|| state.locale().to_string()),
            dir: state.direction_explicit().then(|| state.direction()),
        };
        self.inner
            .renderer_port
            .set_localization(state as Rc<dyn Translator>, attributes);
    }

    /// The localization options the chart was built with (unresolved).
    pub fn localization(&self) -> LocalizationOptions {
        self.inner.locale.borrow().options().clone()
    }

    // ── indicators ─────────────────────────────────────────

    /// Register a scripting engine so `add_indicator` with `language` can run that
    /// language. Vela ships NO engine: install the one you need and register it;
    /// without one the chart displays candles, drawings and native indicators
    /// only. Re-registering a language replaces it.
    pub fn register_engine(&self, language: &str, engine: Rc<dyn ScriptingEngine>) -> &Self {
        self.inner.orchestrator.register_engine(language, engine);
        self
    }

    /// Run an indicator script on the chart's market data and render it. The
    /// handle is usable at once; data renders when execution resolves. A
    /// host-supplied id that is empty or already live is refused.
    pub fn add_indicator(
        &self,
        source: &str,
        options: AddIndicatorOptions,
    ) -> Result<IndicatorHandleRef, VelaError> {
        let handle = self.inner.orchestrator.add_indicator(source, options)?;
        Ok(handle)
    }

    /// Add a built-in NATIVE indicator (core-computed, no scripting engine) by
    /// registered `type`, e.g. `vpvr` or `sma`. It becomes a first-class indicator
    /// (legend row, settings, hide, remove, events). Layer-backed types (`volume`,
    /// `vpvr`) are single-instance: a second call returns the existing handle; the
    /// classic studies allow several instances. Returns a fail-soft handle for an
    /// unregistered type.
    pub fn add_native_indicator(
        &self,
        indicator_type: &str,
        options: NativeIndicatorOptions,
    ) -> IndicatorHandleRef {
        self.inner
            .orchestrator
            .add_native_indicator(indicator_type, options)
    }

    /// The catalog of built-in native indicators with their live state on this
    /// chart: each entry's `type`, `title`, whether it is `supported` on the
    /// current symbol, whether it is already `present`, and whether it allows
    /// several instances. Async because support may probe the provider.
    pub fn available_native_indicators(&self) -> LocalBoxFuture<'static, Vec<NativeIndicatorInfo>> {
        self.inner.orchestrator.available_native_indicators()
    }

    /// The native-indicator types PRESENT on the chart right now, one entry per
    /// instance (a multi-instance type repeats): the synchronous slice of
    /// [`available_native_indicators`](Self::available_native_indicators).
    pub fn present_native_indicators(&self) -> Vec<String> {
        self.inner.orchestrator.present_native_indicators()
    }

    /// The native-indicator types the chart is about to add on its own and has
    /// not yet: the volume indicator while the first market's symbol facts, which
    /// decide it, are still in flight. Empty otherwise.
    pub fn pending_native_indicators(&self) -> Vec<String> {
        self.inner.orchestrator.pending_native_indicators()
    }

    /// The facts the chart applies about its symbol, from its data source's
    /// `SymbolInfo`; `None` while unknown. They change with `symbol:facts`.
    pub fn symbol_facts(&self) -> Option<SymbolFacts> {
        self.inner.orchestrator.symbol_facts()
    }

    /// Live handles of every indicator currently on the chart (script + native).
    pub fn indicators(&self) -> Vec<IndicatorHandleRef> {
        self.inner
            .orchestrator
            .list_indicators()
            .into_iter()
            .map(|h| h as IndicatorHandleRef)
            .collect()
    }

    /// Execute an indicator script and INJECT it only if the run succeeds: the
    /// seam for host editors and consoles. Resolves `ok` after the first
    /// successful evaluation, or `!ok` after a compile/runtime failure, in which
    /// case the failed indicator is removed again (no dead legend row). Never
    /// fails.
    pub fn run_indicator(
        &self,
        source: &str,
        options: AddIndicatorOptions,
    ) -> LocalBoxFuture<'static, RunIndicatorResult> {
        let handle = match self.add_indicator(source, options) {
            Ok(handle) => handle,
            Err(err) => {
                return Box::pin(std::future::ready(RunIndicatorResult {
                    ok: false,
                    handle: None,
                    error: Some(err.to_string()),
                    context: None,
                }));
            }
        };
        let (tx, rx) = oneshot::<RunIndicatorResult>();
        let settled = Rc::new(Cell::new(false));
        let off_ready: Slot = Rc::new(RefCell::new(None));
        let off_error: Slot = Rc::new(RefCell::new(None));
        let platform = Rc::clone(&self.inner.env.platform);
        let on_ready = {
            let (tx, settled, handle) = (tx.clone(), Rc::clone(&settled), Rc::clone(&handle));
            let (a, b) = (Rc::clone(&off_ready), Rc::clone(&off_error));
            Box::new(move |_: &IndicatorEvent| {
                if settled.replace(true) {
                    return;
                }
                unsubscribe(&a);
                unsubscribe(&b);
                tx.send(RunIndicatorResult {
                    ok: true,
                    handle: Some(Rc::clone(&handle)),
                    error: None,
                    context: None,
                });
            })
        };
        let on_error = {
            let handle = Rc::clone(&handle);
            let (a, b) = (Rc::clone(&off_ready), Rc::clone(&off_error));
            Box::new(move |event: &IndicatorEvent| {
                let IndicatorEvent::Error { error } = event else {
                    return;
                };
                if settled.replace(true) {
                    return;
                }
                unsubscribe(&a);
                unsubscribe(&b);
                let (tx, handle, error) = (tx.clone(), Rc::clone(&handle), error.clone());
                platform.spawn(Box::pin(async move {
                    // A context that cannot be read is no context.
                    let context = handle.context(None).await.ok().flatten();
                    handle.remove(); // a failed script leaves no dead legend row
                    tx.send(RunIndicatorResult {
                        ok: false,
                        handle: None,
                        error: Some(error),
                        context,
                    });
                }));
            })
        };
        let off = handle.on(IndicatorEventName::Ready, on_ready);
        *off_ready.borrow_mut() = Some(off);
        let off = handle.on(IndicatorEventName::Error, on_error);
        *off_error.borrow_mut() = Some(off);
        Box::pin(rx)
    }

    /// Execute a script and resolve its FIRST computed run: the data-out door for
    /// host editors, consoles and dashboards. The script is injected only if it
    /// runs (a failure removes it again), and the result carries the run itself
    /// plus the controls for what it put on the chart: `on_update` to follow later
    /// runs, `remove` to take it off. Never fails.
    ///
    /// `run_script` is `run_indicator` with the run as its payload rather than a
    /// handle to go fetch from, as `script:run` relates to `context:changed`.
    pub fn run_script(
        &self,
        source: &str,
        options: AddIndicatorOptions,
    ) -> LocalBoxFuture<'static, ScriptRunResult> {
        let handle = match self.add_indicator(source, options) {
            Ok(handle) => handle,
            Err(err) => {
                // A rejected `options.id` (already live): the "never fails" contract
                // turns it into a structured failure, and nothing was mounted.
                return Box::pin(std::future::ready(ScriptRunResult::failed(err.to_string())));
            }
        };
        let updates: Updates = Rc::new(RefCell::new(Vec::new()));
        let next_id = Rc::new(Cell::new(0u64));
        let (tx, rx) = oneshot::<ScriptRunResult>();
        let settled = Rc::new(Cell::new(false));
        let off_run: Slot = Rc::new(RefCell::new(None));
        let off_error: Slot = Rc::new(RefCell::new(None));
        // Resolving on the RUN, not on `ready`: the handle is announced while the
        // model mounts, whereas the run is assembled a turn later (its
        // execution-context pull may cross a worker). Waiting for the run is what
        // makes `result.run` present. The subscription outlives the resolution: it
        // feeds `on_update` too.
        let on_run = {
            let handle = Rc::clone(&handle);
            let (updates, settled, tx) = (Rc::clone(&updates), Rc::clone(&settled), tx.clone());
            let (off_run, off_error) = (Rc::clone(&off_run), Rc::clone(&off_error));
            move |event: &ChartEvent| {
                let ChartEvent::ScriptRun(run) = event else {
                    return;
                };
                if run.id != handle.id() {
                    return;
                }
                if settled.get() {
                    let listeners: Vec<_> =
                        updates.borrow().iter().map(|(_, h)| Rc::clone(h)).collect();
                    for listener in listeners {
                        if let Ok(mut f) = listener.try_borrow_mut() {
                            f(run);
                        }
                    }
                    return;
                }
                settled.set(true);
                unsubscribe(&off_error);
                let follow = {
                    let (updates, next_id) = (Rc::clone(&updates), Rc::clone(&next_id));
                    move |handler: ScriptRunHandler| -> Unsubscribe {
                        let id = next_id.get();
                        next_id.set(id + 1);
                        updates
                            .borrow_mut()
                            .push((id, Rc::new(RefCell::new(handler))));
                        let updates = Rc::clone(&updates);
                        Box::new(move || updates.borrow_mut().retain(|(i, _)| *i != id))
                    }
                };
                let take_off = {
                    let (updates, handle, off_run) =
                        (Rc::clone(&updates), Rc::clone(&handle), Rc::clone(&off_run));
                    move || {
                        unsubscribe(&off_run);
                        updates.borrow_mut().clear();
                        handle.remove();
                    }
                };
                tx.send(ScriptRunResult::succeeded(run.clone(), follow, take_off));
            }
        };
        let on_error = {
            let handle = Rc::clone(&handle);
            let (off_run, off_error) = (Rc::clone(&off_run), Rc::clone(&off_error));
            Box::new(move |event: &IndicatorEvent| {
                let IndicatorEvent::Error { error } = event else {
                    return;
                };
                if settled.replace(true) {
                    return;
                }
                unsubscribe(&off_run);
                unsubscribe(&off_error);
                handle.remove(); // a failed script leaves no dead legend row
                tx.send(ScriptRunResult::failed(error.clone()));
            })
        };
        let off = self.listen(ChartEventName::ScriptRun, on_run);
        *off_run.borrow_mut() = Some(off);
        let off = handle.on(IndicatorEventName::Error, on_error);
        *off_error.borrow_mut() = Some(off);
        Box::pin(rx)
    }

    // ── market ─────────────────────────────────────────────

    /// Switch the chart's market IN PLACE (symbol, provider, timeframe, depth, or
    /// offline data) WITHOUT destroying the chart: indicators re-execute over the
    /// new bars, native indicators restart, and panes, user drawings, renderer
    /// config and event subscriptions all survive. Resolves once the new market's
    /// history is painted (a deep backfill continues behind it: await
    /// [`history_complete`](Self::history_complete)); a call superseded by a newer
    /// `set_market` resolves silently. Emits `market:changed` (with the previous
    /// identity) when the market identity changed. The switch starts when this is
    /// CALLED.
    pub fn set_market(&self, next: MarketSwitch) -> LocalBoxFuture<'static, Result<(), VelaError>> {
        let load = self.inner.orchestrator.set_market(next);
        Box::pin(async move { load.await.map_err(VelaError::from) })
    }

    /// The current market identity: the read counterpart of
    /// [`set_market`](Self::set_market). A snapshot of the REQUESTED market: it
    /// reflects an in-flight switch immediately. Listen to `market:changed` for
    /// committed identity changes.
    pub fn market(&self) -> MarketSnapshot {
        self.inner.orchestrator.market_snapshot()
    }

    /// Resolves once the chart is painted and interactive. For a symbol-backed
    /// chart this awaits a provider being registered that resolves the symbol (the
    /// parked load). On a ranged feed the first paint is a small recent head and
    /// the rest of the history keeps backfilling BEHIND this: await
    /// [`history_complete`](Self::history_complete) for the full depth. Distinct
    /// from `chart.data().ready()`, which awaits only the provider symbol indexes.
    pub fn ready(&self) -> LocalBoxFuture<'static, Result<(), VelaError>> {
        let ready = self.inner.orchestrator.ready();
        Box::pin(async move { ready.await.map_err(VelaError::from) })
    }

    /// Resolves once the FULL requested history has loaded (at once for small or
    /// offline charts; after the backward backfill for deep ones: see the
    /// `history:progress` / `history:complete` events). Never fails: on destroy or
    /// a failed backfill it resolves with whatever depth loaded.
    pub fn history_complete(&self) -> LocalBoxFuture<'static, ()> {
        self.inner.orchestrator.history_complete()
    }

    /// A renderer-agnostic snapshot of the graphic elements the core has generated
    /// (series, fills, drawings, tables, ...): a deterministic check that a feature
    /// was produced, independent of which renderer drew it.
    pub fn inspect(&self) -> SceneInspection {
        self.inner.orchestrator.inspect()
    }

    /// The host's tab-restore signal (the original listens to `visibilitychange`):
    /// backfills any bars missed while the page was hidden.
    pub fn on_page_visible(&self) {
        self.inner.orchestrator.on_page_visible();
    }

    // ── control surfaces ───────────────────────────────────

    /// The active renderer's control surface. Set/read render features at runtime
    /// with no indicator re-run. Unsupported keys warn and no-op.
    pub fn renderer(&self) -> &RendererControl {
        &self.inner.renderer_control
    }

    /// The chart's pane control surface: list panes with the indicators each holds
    /// and move/merge/reorder/collapse/maximize them. On a renderer without pane
    /// management the mutators warn and no-op.
    pub fn panes(&self) -> &PanesControl {
        &self.inner.panes_control
    }

    /// The chart's data control surface: register market-data providers and query
    /// the registry. No provider is bundled; registering the one that resolves the
    /// chart symbol fires the parked initial load (await it with
    /// [`ready`](Self::ready)).
    pub fn data(&self) -> &DataControl {
        &self.inner.data_control
    }

    /// The chart's user-drawings control surface. Always present; on a renderer
    /// without the `userDrawings` capability the interactive methods warn and
    /// no-op while persistence still round-trips.
    pub fn drawings(&self) -> &DrawingsControl {
        &self.inner.drawings_control
    }

    /// The chart's timeline-marks control surface: host events pinned to a bar and
    /// shown as glyphs on a lane above the time axis. Marks are data, not user
    /// state: re-supply them on `market:changed`.
    pub fn marks(&self) -> &MarksControl {
        &self.inner.marks_control
    }

    /// The chart's bar-replay control surface: rewind to a past bar and reveal the
    /// following ones by hand or on a timer, from the history already loaded.
    /// Revealed bars behave like live bars for indicators; live updates pause
    /// meanwhile. Follow it with the `replay:*` events.
    pub fn replay(&self) -> &ReplayControl {
        &self.inner.replay_control
    }

    // ── events ─────────────────────────────────────────────

    /// Listen to a chart event. Handlers run in registration order; one may call
    /// back into the chart. Detach with [`off`](Self::off).
    pub fn on(
        &self,
        event: ChartEventName,
        handler: impl Fn(&ChartEvent) + 'static,
    ) -> Subscription<ChartEventName> {
        self.inner.orchestrator.events().on(event, handler)
    }

    /// Detach a handler registered with [`on`](Self::on). Returns whether it was
    /// still registered.
    pub fn off(&self, subscription: &Subscription<ChartEventName>) -> bool {
        self.inner.orchestrator.events().off(subscription)
    }

    /// [`on`](Self::on) as the original's unsubscribe function.
    fn listen(
        &self,
        event: ChartEventName,
        handler: impl Fn(&ChartEvent) + 'static,
    ) -> Unsubscribe {
        let bus = self.inner.orchestrator.events();
        let subscription = bus.on(event, handler);
        Box::new(move || {
            bus.off(&subscription);
        })
    }

    // ── view ───────────────────────────────────────────────

    /// The current visible time range (`from`/`to` in epoch-ms), or `None` before
    /// data loads.
    pub fn get_visible_range(&self) -> Option<VisibleRange> {
        self.inner.orchestrator.get_visible_range()
    }

    /// Set the visible time range explicitly (epoch-ms). Use for a custom date
    /// range.
    pub fn set_visible_range(&self, range: VisibleRange) -> &Self {
        self.inner.orchestrator.set_visible_range(range);
        self
    }

    /// Pan the view by a fraction of the visible width: positive toward the
    /// latest bars, negative into history. Behaves like dragging the chart.
    pub fn pan_by(&self, fraction: f64) -> &Self {
        self.inner.orchestrator.pan_by(fraction);
        self
    }

    /// Frame a named date-range preset over the loaded bars (`1D`, `1W`, `1M`,
    /// `3M`, `6M`, `1Y`, `YTD`, `ALL`). A preset deeper than the loaded history
    /// frames everything (it does not fetch more bars).
    pub fn set_visible_range_preset(&self, preset: VisibleRangePreset) -> &Self {
        self.inner.orchestrator.set_visible_range_preset(preset);
        self
    }

    pub fn resize(&self) {
        self.inner.orchestrator.resize();
    }

    /// Swap the app theme at runtime: `"dark"`, `"light"`, a registered theme id,
    /// a full custom theme, or a `createTheme` result. Re-skins the chart surface,
    /// axes, legends and in-chart chrome live (no indicator re-run, no rebuild) and
    /// emits `theme:changed` with the resolved theme so host chrome can follow.
    pub fn set_theme(&self, theme: impl Into<ThemeSpec>) -> &Self {
        let resolved = self
            .inner
            .env
            .registries
            .themes
            .borrow()
            .resolve(Some(&theme.into()));
        self.inner.orchestrator.set_theme(resolved);
        self
    }

    /// Tear the chart down: stops loads, timers, live updates, indicators and the
    /// renderer, and drops every event handler. Idempotent. Dropping the last
    /// handle does the same.
    pub fn destroy(&self) {
        if !self.inner.destroyed.replace(true) {
            self.inner.orchestrator.destroy();
        }
    }
}

// ── plumbing ───────────────────────────────────────────────

/// A subscription held by the handler it belongs to, taken when it settles.
type Slot = Rc<RefCell<Option<Unsubscribe>>>;

/// The listeners `run_script`'s `on_update` registered.
type Updates = Rc<RefCell<Vec<(u64, Rc<RefCell<ScriptRunHandler>>)>>>;

/// Call the unsubscribe a slot holds, once. The slot's borrow ends before the call.
fn unsubscribe(slot: &Slot) {
    let off = slot.borrow_mut().take();
    if let Some(off) = off {
        off();
    }
}
