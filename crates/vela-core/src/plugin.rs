//! The Vela PLUGIN SDK: the public extension surface a plugin package builds against.
//! Everything here is framework-free and side-effect-free to use.
//!
//! Extension seams:
//!
//! - CHART TYPES ([`PluginHost::register_chart_type`]): a new price style contributing a bar
//!   transform, a per-chart data engine and an extended-ticker modifier.
//! - NATIVE INDICATORS ([`PluginHost::register_native_indicator`]): core-computed indicators
//!   with renderer layers driven through the same native-data channels (volume and VPVR are
//!   built on it).
//! - SCRIPTING ENGINES (`Vela::register_engine`; [`Contributions::register_default_engine`]
//!   for shells): a language runtime executing indicator scripts against Vela-owned bars
//!   through the [`ScriptingEngine`] port.
//! - WIDGET CONTRIBUTIONS ([`Contributions`]): actions, attachments, side panels, legend
//!   actions and callouts, state persistence, symbol ranking. Contributions are DATA
//!   descriptors, never DOM.
//! - Defaults other registries read at construction: renderer defaults and shell defaults.
//!
//! # Differences from the original
//!
//! The original registers into module globals; here a [`PluginHost`] carries the registries,
//! the way [`VelaEnv`] carries the engine's: the chart-type, native-indicator, drawing and
//! theme registries and the renderer defaults are the environment's own (charts built from
//! clones of the environment see a late registration), and the widget's contribution
//! registries and shell defaults live beside them. Hand the host's pieces to the shells you
//! build. The names follow the original's with Rust casing; what the original re-exports
//! from other modules is re-exported here from where the port keeps it.
//!
//! Not part of this crate's surface: `registerIcon` / `iconMarkup` (the icon registry is the
//! browser kit's) and `DataUnavailableError` helpers beyond [`crate::data_unavailable`].

use std::rc::Rc;

use serde_json::Value;

use crate::indicators::{NativeIndicatorDescriptor, NativeIndicatorRegistry};
use crate::renderer_defaults::DefaultsRegistration;
use crate::runtime::Platform;
use crate::vela::VelaEnv;
use crate::widget::contributions::Contributions;
use crate::widget::shell_defaults::ShellDefaultsRegistry;

// LOCALIZATION: `msg()` declares a translatable message (stable id + English text), a
// `LocalePack` carries one language's messages and the locale registry adds it. Every chart,
// workspace and widget context carries a `Translator` (`chart.i18n()`, `ctx.i18n()`) that
// resolves messages and formats numbers and dates for the chart's locale.
pub use crate::format::{ResolvedFormatters, types::VelaFormatters};
pub use crate::i18n::{
    LocaleChangedEvent, LocalePack, LocaleRegistry, LocalizationOptions, MessageArg,
    MessageDescriptor, MessageLike, TextDirection, Translator, create_translator, msg, with_values,
};

pub use crate::chart_types::{
    BasePainting, ChartTypeDefinition, ChartTypeRegistry, ChartTypeSettingsInstance,
    ChartTypeSettingsSection, ChartTypeSettingsSubsection, NormalizedSettingsRow, SeriesDataEngine,
    SeriesDataEngineHost, SettingsInlineControl, SettingsRowCondition, SettingsRowDescriptor,
    SettingsRowInlineNumber, SettingsRowSwatch, SettingsRowValueKey, SettingsRowWhen,
    SettingsRowWidth, SettingsSelectOption, normalize_settings_row, price_style_applies,
    settings_row_value_keys, shown_price_style,
};
pub use crate::drawings::registry::{DrawingRegistry, DrawingTypeMeta};
pub use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
pub use crate::ports::market_data::{MarketDataFeed, SymbolFacts, SymbolInfo};
pub use crate::ports::scripting_engine::{
    EngineCapabilities, ExecutionHandlers, ExecutionRequest, ExecutionSession, PreparedScript,
    ScriptingEngine,
};
pub use crate::price_styles::BarTransform;
pub use crate::script_run::{ScriptRun, ScriptRunResult};
// The semantic palette (fixed brand/meaning colors, never theme-dependent) so plugin output
// matches core affordances exactly.
pub use crate::palette::*;

pub use crate::ui::keymap::{KeyBindingDescriptor, ResolvedBinding};
pub use crate::widget::context_menu_model::CONTEXT_MENU_BUILTIN_ORDER;
pub use crate::widget::contributions::{
    ActionAlign, CellStateContext, ContextMenuPointer, DEFAULT_PANEL_ORDER, DockEdge, DockPosition,
    DockStripOptions, DockedPanelInfo, DockedStrip, ElementHandle, ExternalIndicatorEntry,
    LegendActionDescriptor, LegendCalloutContent, LegendCalloutDescriptor, LegendCalloutItem,
    LegendCalloutSpec, LegendIndicatorInfo, MobilePlacement, OVERRIDABLE_TOPBAR_IDS, PanelDockView,
    PanelHeaderMode, PanelPlacement, PointerPaneKind, RankedPool, SidePanelButton,
    SidePanelDescriptor, SidePanelHandle, SidePanelHeader, SidebarBadge, SidebarControl,
    StatePersistenceHandler, StateScope, SymbolRankingHook, ToastKind, TopbarItemId, TopbarItems,
    WidgetActionDescriptor, WidgetActionTarget, WidgetAttachment, WidgetContext, mobile_placement,
};
pub use crate::widget::shell_defaults::{ShellDefaultKey, ShellDefaults, SidebarOption};
pub use crate::widget::shell_options::EngineFactory;
pub use crate::widget::sidebar_composition::{
    SIDEBAR_DEFAULT_BOTTOM, SIDEBAR_DEFAULT_TOP, SidebarComposition, compose_sidebar,
};
// A plugin panel gets the shell's own column; these bounds are what `resizable` clamps to.
pub use crate::widget::side_panel::{
    DEFAULT_PANEL_MAX_WIDTH, DEFAULT_PANEL_MIN_WIDTH, DEFAULT_PANEL_WIDTH, SidePanelOptions,
    clamp_panel_width,
};

/// The registries a plugin registers into: the environment's (chart types, native indicators,
/// drawing tools, themes, locale packs, renderer defaults) and the widget's contributions and
/// shell defaults. Clones share everything.
#[derive(Clone)]
pub struct PluginHost {
    env: VelaEnv,
    contributions: Contributions,
    shell_defaults: ShellDefaultsRegistry,
}

impl PluginHost {
    /// A host over `env`; its contribution registries start empty.
    pub fn new(env: VelaEnv) -> Self {
        let contributions = Contributions::new(Rc::clone(&env.platform));
        Self {
            env,
            contributions,
            shell_defaults: ShellDefaultsRegistry::new(),
        }
    }

    /// The environment the engine's registries live in. Build charts from it.
    pub fn env(&self) -> &VelaEnv {
        &self.env
    }

    /// The widget contribution registries: pass them to the shells you build.
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }

    /// The shell option defaults: pass them to the shells you build.
    pub fn shell_defaults(&self) -> &ShellDefaultsRegistry {
        &self.shell_defaults
    }

    /// The platform the environment runs on.
    pub fn platform(&self) -> &Rc<dyn Platform> {
        &self.env.platform
    }

    // ── chart types ──

    /// Register (or replace) a chart type. Charts built from the environment, and the ones
    /// already built, see it.
    pub fn register_chart_type(&self, def: ChartTypeDefinition) {
        self.env.registries.chart_types.borrow_mut().register(def);
    }

    pub fn unregister_chart_type(&self, id: &str) {
        self.env.registries.chart_types.borrow_mut().unregister(id);
    }

    /// The ids of the registered chart types.
    pub fn chart_types(&self) -> Vec<String> {
        self.env
            .registries
            .chart_types
            .borrow()
            .all()
            .iter()
            .map(|d| d.id.clone())
            .collect()
    }

    /// The ids that name extended-ticker modifiers (`"SYM;id"` for scripts).
    pub fn ticker_modifier_ids(&self) -> Vec<String> {
        self.env
            .registries
            .chart_types
            .borrow()
            .ticker_modifier_ids()
    }

    // ── native indicators ──

    /// Register (or replace) a native indicator.
    pub fn register_native_indicator(&self, descriptor: NativeIndicatorDescriptor) {
        self.env
            .registries
            .native_indicators
            .borrow_mut()
            .register(descriptor);
    }

    /// Whether something was registered under the type.
    pub fn unregister_native_indicator(&self, indicator_type: &str) -> bool {
        self.env
            .registries
            .native_indicators
            .borrow_mut()
            .unregister(indicator_type)
    }

    /// The registered native-indicator types.
    pub fn native_indicator_types(&self) -> Vec<String> {
        let reg: std::cell::Ref<'_, NativeIndicatorRegistry> =
            self.env.registries.native_indicators.borrow();
        reg.types().into_iter().map(str::to_string).collect()
    }

    // ── renderer defaults ──

    /// Register renderer feature defaults every chart built afterwards applies once its
    /// renderer is mounted. Dispose the registration to remove exactly what it set.
    pub fn register_renderer_defaults(
        &self,
        values: impl IntoIterator<Item = (String, Value)>,
    ) -> DefaultsRegistration {
        self.env.renderer_defaults.borrow_mut().register(values)
    }

    pub fn dispose_renderer_defaults(&self, registration: &DefaultsRegistration) {
        self.env
            .renderer_defaults
            .borrow_mut()
            .dispose(registration);
    }

    pub fn unregister_renderer_defaults(&self, keys: &[&str]) {
        self.env.renderer_defaults.borrow_mut().unregister(keys);
    }

    /// The registered renderer defaults, in registration order.
    pub fn renderer_defaults(&self) -> Vec<(String, Value)> {
        self.env.renderer_defaults.borrow().snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    fn host() -> PluginHost {
        PluginHost::new(VelaEnv::new(ManualPlatform::new(0.0).handle()))
    }

    #[test]
    fn a_chart_type_registers_replaces_and_unregisters_through_the_environment() {
        let h = host();
        let builtin = h.chart_types().len();
        h.register_chart_type(ChartTypeDefinition::new("renko"));
        assert!(h.chart_types().contains(&"renko".to_string()));
        h.register_chart_type(ChartTypeDefinition::new("renko"));
        assert_eq!(
            h.chart_types().len(),
            builtin + 1,
            "re-registering an id replaces it"
        );
        // The environment's own registry is what charts read.
        assert!(
            h.env()
                .registries
                .chart_types
                .borrow()
                .get("renko")
                .is_some()
        );
        h.unregister_chart_type("renko");
        assert!(!h.chart_types().contains(&"renko".to_string()));
        assert!(h.ticker_modifier_ids().iter().all(|id| id != "renko"));
    }

    #[test]
    fn clones_of_a_host_and_of_its_environment_share_the_registries() {
        let h = host();
        let other = h.clone();
        other.register_chart_type(ChartTypeDefinition::new("kagi"));
        assert!(h.chart_types().contains(&"kagi".to_string()));
        let sibling_env = h.env().clone();
        assert!(
            sibling_env
                .registries
                .chart_types
                .borrow()
                .get("kagi")
                .is_some()
        );
    }

    #[test]
    fn the_builtin_native_indicators_are_listed_and_unknown_types_unregister_to_false() {
        let h = host();
        let types = h.native_indicator_types();
        assert!(types.contains(&"volume".to_string()) && types.contains(&"vpvr".to_string()));
        assert!(!h.unregister_native_indicator("nope"));
        assert!(h.unregister_native_indicator("vpvr"));
        assert!(!h.native_indicator_types().contains(&"vpvr".to_string()));
    }

    #[test]
    fn renderer_defaults_register_and_dispose_only_their_own_values() {
        let h = host();
        assert!(h.renderer_defaults().is_empty());
        let first = h.register_renderer_defaults([("glow".to_string(), json!(0.6))]);
        let _second = h.register_renderer_defaults([
            ("glow".to_string(), json!(0.9)),
            ("grid".to_string(), json!(false)),
        ]);
        h.dispose_renderer_defaults(&first); // stale: the key now carries the second value
        assert_eq!(h.renderer_defaults().len(), 2);
        h.unregister_renderer_defaults(&["glow"]);
        assert_eq!(h.renderer_defaults(), [("grid".to_string(), json!(false))]);
    }

    #[test]
    fn contributions_and_shell_defaults_ride_the_host() {
        let h = host();
        let off = h
            .contributions()
            .register_widget_action(WidgetActionDescriptor::new(
                "vendor.tool",
                WidgetActionTarget::Topbar,
                "Tool",
                |_| {},
            ));
        assert_eq!(
            h.contributions()
                .widget_actions(WidgetActionTarget::Topbar, None)
                .len(),
            1
        );
        off();
        let _ = h.shell_defaults().register(ShellDefaults {
            sidebar: Some(SidebarOption::Enabled(true)),
        });
        assert_eq!(
            h.shell_defaults().defaults().sidebar,
            Some(SidebarOption::Enabled(true))
        );
        // The refusal of a composite built-in id warns through the environment's platform.
        let refused = h
            .contributions()
            .register_widget_action(WidgetActionDescriptor::new(
                "symbol",
                WidgetActionTarget::Topbar,
                "No",
                |_| {},
            ));
        refused();
        assert!(
            h.contributions()
                .widget_actions(WidgetActionTarget::Topbar, None)
                .is_empty()
        );
        let _ = h.platform();
    }

    #[test]
    fn the_surface_exposes_the_original_constants() {
        assert_eq!(OVERRIDABLE_TOPBAR_IDS, ["indicators", "screenshot"]);
        assert_eq!(DEFAULT_PANEL_ORDER, 100.0);
        assert_eq!(
            (
                DEFAULT_PANEL_WIDTH,
                DEFAULT_PANEL_MIN_WIDTH,
                DEFAULT_PANEL_MAX_WIDTH
            ),
            (280.0, 200.0, 640.0)
        );
        assert_eq!(clamp_panel_width(1.0, 200.0, 640.0), 200.0);
        assert_eq!(CONTEXT_MENU_BUILTIN_ORDER.len(), 3);
        assert_eq!(SIDEBAR_DEFAULT_TOP, ["panels"]);
        assert_eq!(msg("plugin.test", "Hello").source_text(), "Hello");
    }
}
