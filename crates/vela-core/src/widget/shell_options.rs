//! The SHELL options: the one option vocabulary both batteries-included shells share.
//! The widget's options are the chart options plus this; the workspace's are the same minus
//! `height`, plus the grid options, and at the workspace every chart option given top-level
//! becomes the DEFAULT of each cell. Every name below means the SAME thing in both shells;
//! per-shell multiplicity (one chart vs N cells) is noted per field, never a semantic fork.

use std::collections::HashMap;
use std::rc::Rc;

use super::indicators::IndicatorsSource;
use super::layout_mode::LayoutModeOption;
use super::persist::VelaStorage;
use super::shell_defaults::SidebarOption;
use super::topbar_composition::TopbarComposition;
use crate::ports::data_provider::DataProvider;
use crate::ports::scripting_engine::ScriptingEngine;

/// A provider factory: the shell owns the call cycle (the widget re-instantiates on each
/// chart rebuild; the workspace instantiates once onto its single shared feed).
pub type ProviderFactory = Rc<dyn Fn() -> Rc<dyn DataProvider>>;

/// A scripting-engine factory: ONE instance per chart (the widget's chart, each workspace
/// cell), so a worker engine gets its own thread and dies with its chart. Return a shared
/// instance from the factory to opt into one engine for everything.
pub type EngineFactory = Rc<dyn Fn() -> Rc<dyn ScriptingEngine>>;

/// The `persist` option: `true` uses the shell's default key (`vela-widget` /
/// `vela-workspace`); a string is the storage key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistOption {
    Enabled(bool),
    Key(String),
}

/// What a shell (widget or workspace) accepts BEYOND the chart options themselves.
#[derive(Clone, Default)]
pub struct VelaShellOptions {
    /// Provider factories, keyed by provider name.
    pub providers: Option<HashMap<String, ProviderFactory>>,
    /// Scripting-engine factories, keyed by language. Merged OVER the app-level
    /// default-engine registry: an instance factory wins for its language.
    pub engines: Option<HashMap<String, EngineFactory>>,
    /// Indicator manifest: inline, a URL returning it, or an ASYNC LOADER. Resolved ONCE;
    /// entries with `enabled: true` auto-add to every FRESH chart (restored cells re-add
    /// their own recorded set instead).
    pub indicators: Option<IndicatorsSource>,
    /// Topbar timeframe presets (chart timeframe values).
    pub timeframes: Option<Vec<String>>,
    /// Display timezone, one choice for the whole shell: an IANA zone (default
    /// `"Etc/UTC"`), or `"exchange"` to render every chart in its own market's zone.
    pub timezone: Option<String>,
    /// Chrome toggles (all default true).
    pub statusline: Option<bool>,
    pub watermark: Option<bool>,
    pub bottombar: Option<bool>,
    /// Declarative topbar composition: the VISIBLE entries per side, in render order.
    pub topbar: Option<TopbarComposition>,
    /// A toolbar along the shell's right edge (default off). `true` uses the default
    /// layout; a composition lists the entries per section. Desktop only.
    pub sidebar: Option<SidebarOption>,
    /// The built-in indicator picker's entry points. `false` removes them.
    ///
    /// Deprecated in the original (removed in 0.7.0): omit `"indicators"` from the topbar's
    /// left list to hide the surface, or register a widget action under the id
    /// `"indicators"` to replace it.
    pub indicator_picker: Option<bool>,
    /// Chrome size class. `Auto` (default) follows the CONTAINER width plus a
    /// coarse-pointer heuristic; `Mobile` / `Desktop` pin it.
    pub layout_mode: Option<LayoutModeOption>,
    /// Focus the chart when the shell mounts so keyboard shortcuts work from the first
    /// keystroke. Default false: an embedded shell must never steal the page's focus.
    pub autofocus: Option<bool>,
    /// Bring the shell back AS YOU LEFT IT: persist the full state document and restore it
    /// at construction.
    pub persist: Option<PersistOption>,
    /// Storage backend for `persist` (the browser crate supplies `localStorage` by
    /// default). Inject any [`VelaStorage`] for custom backends.
    pub storage: Option<Rc<dyn VelaStorage>>,
}

impl VelaShellOptions {
    /// The storage key `persist` resolves to, given the shell's own default key.
    pub fn persist_key(&self, default_key: &str) -> Option<String> {
        match &self.persist {
            Some(PersistOption::Key(k)) => Some(k.clone()),
            Some(PersistOption::Enabled(true)) => Some(default_key.to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_resolves_to_the_default_key_a_custom_key_or_nothing() {
        let mut o = VelaShellOptions::default();
        assert_eq!(o.persist_key("vela-widget"), None);
        o.persist = Some(PersistOption::Enabled(false));
        assert_eq!(o.persist_key("vela-widget"), None);
        o.persist = Some(PersistOption::Enabled(true));
        assert_eq!(o.persist_key("vela-widget").as_deref(), Some("vela-widget"));
        o.persist = Some(PersistOption::Key("mine".into()));
        assert_eq!(o.persist_key("vela-widget").as_deref(), Some("mine"));
    }
}
