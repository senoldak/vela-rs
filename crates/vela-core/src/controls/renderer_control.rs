//! The public control surface for the active renderer, `chart.renderer`: a thin
//! facade over the renderer port. Get, set and inspect render features without
//! exposing the internal orchestration methods (`set_bars`, `mount_indicator`,
//! ...). A key the active renderer does not support warns (through the platform)
//! and is ignored: the chart is never touched.
//!
//! Differences from the original:
//! - `screenshotCanvas(): HTMLCanvasElement | null` is left out (DOM), as in the
//!   renderer port.
//! - The port has no `supports_get_config`: a renderer that returns `None` from
//!   `get_config` is read as having no rich config (and warned about).
//! - `set(feature | record)` is [`RendererControl::set`] for one feature and
//!   [`RendererControl::set_many`] for several.
//! - `setExternalCrosshair(time, price = null)` takes the price as an `Option`.

use std::rc::Rc;

use serde_json::Value;

use crate::model::Millis;
use crate::ports::chart_renderer::{
    AxisLongPressEvent, ChartRenderer, ClickEvent, CrosshairEvent, DataWindowReadout, LayoutMode,
    LegendActionProvider, LegendCalloutProvider, Listener, RendererCapabilities, SettingsSection,
    SettingsVisibility, SymbolPickerFn, WallClock,
};
use crate::runtime::{Platform, Unsubscribe, noop_unsubscribe};

/// `chart.renderer`. A cheap handle.
#[derive(Clone)]
pub struct RendererControl {
    renderer: Rc<dyn ChartRenderer>,
    platform: Rc<dyn Platform>,
}

impl RendererControl {
    /// `platform` receives the warnings for features the renderer lacks.
    pub fn new(renderer: Rc<dyn ChartRenderer>, platform: Rc<dyn Platform>) -> Self {
        Self { renderer, platform }
    }

    /// The active renderer's identity, e.g. `native` or `lwc`.
    pub fn name(&self) -> &str {
        self.renderer.name()
    }

    /// What the active renderer can draw (drives graceful degradation).
    pub fn capabilities(&self) -> RendererCapabilities {
        self.renderer.capabilities()
    }

    /// Whether the active renderer supports a feature: use to show or hide a UI control.
    pub fn supports(&self, feature: &str) -> bool {
        self.renderer.features().iter().any(|f| f == feature)
    }

    /// Read a feature's current value (`None` if unsupported).
    pub fn get(&self, feature: &str) -> Option<Value> {
        self.renderer.read_feature(feature)
    }

    /// Set one feature (`set("glow", json!(0.6))`). A key the active renderer does
    /// not support emits a warning and is ignored, with no effect on the UI.
    pub fn set(&self, feature: &str, value: Value) -> &Self {
        if self.supports(feature) {
            self.renderer.apply_feature(feature, &value);
        } else {
            self.platform.warn(&format!(
                "[vela] renderer \"{}\" has no feature \"{feature}\" \u{2014} set ignored.",
                self.renderer.name()
            ));
        }
        self
    }

    /// Set several features at once, in order (`set({ logScale: true, upColor: "#fff" })`).
    pub fn set_many(&self, features: impl IntoIterator<Item = (String, Value)>) -> &Self {
        for (key, value) in features {
            self.set(&key, value);
        }
        self
    }

    /// Wire the legend rows' HOST-CONTRIBUTED actions (the shells route the
    /// plugin registry through this; see `registerLegendAction`). Silent on a
    /// renderer without the seam: contributed legend actions simply never show
    /// there, same graceful degradation as the sync ghost crosshair.
    pub fn set_legend_actions(&self, provider: Option<LegendActionProvider>) {
        self.renderer.set_legend_actions(provider);
    }

    /// Wire the legend rows' HOST-CONTRIBUTED callout bubbles (see
    /// `registerLegendCallout`). Silent on a renderer without the seam, like
    /// [`set_legend_actions`](Self::set_legend_actions).
    pub fn set_legend_callouts(&self, provider: Option<LegendCalloutProvider>) {
        self.renderer.set_legend_callouts(provider);
    }

    /// Replace the indicator legend's fold toggle with a host action (or restore
    /// it with `None`): multi-chart shells point the chip at their indicator
    /// overview instead of unfolding rows in place. Silent no-op on a renderer
    /// without a foldable legend.
    pub fn set_legend_overview_action(&self, action: Option<Rc<dyn Fn()>>) -> &Self {
        self.renderer.set_legend_overview_action(action);
        self
    }

    /// Whether the active renderer can open a per-indicator settings dialog.
    pub fn supports_indicator_settings(&self) -> bool {
        self.renderer.supports_open_indicator_settings()
    }

    /// Open one indicator's settings dialog: the programmatic twin of the legend
    /// gear (see [`supports_indicator_settings`](Self::supports_indicator_settings)).
    /// Silent no-op without the seam or for an unknown id.
    pub fn open_indicator_settings(&self, indicator_id: &str) -> &Self {
        self.renderer.open_indicator_settings(indicator_id);
        self
    }

    /// Export the current chart as a PNG data URL, or `None` if the active
    /// renderer does not support it (warns). DOM overlays (tables, legend) are
    /// not included.
    pub fn screenshot(&self) -> Option<String> {
        if self.renderer.supports_screenshot() {
            return self.renderer.screenshot();
        }
        self.platform.warn(&format!(
            "[vela] renderer \"{}\" does not support screenshot().",
            self.renderer.name()
        ));
        None
    }

    /// The active renderer's full cosmetic config as a serializable, versioned
    /// JSON document: persist it (templates, saved settings) and feed it back to
    /// [`apply_config`](Self::apply_config). `None` if the renderer has no rich
    /// config (warns).
    pub fn get_config(&self) -> Option<Value> {
        let config = self.renderer.get_config();
        if config.is_none() {
            self.platform.warn(&format!(
                "[vela] renderer \"{}\" does not support getConfig().",
                self.renderer.name()
            ));
        }
        config
    }

    /// Apply a (possibly partial) config document from [`get_config`](Self::get_config):
    /// load a template or restore saved settings. Malformed or unknown fields are
    /// ignored; no indicator re-run. Does nothing, with a warning, if the renderer
    /// has no rich config.
    pub fn apply_config(&self, config: &Value) -> &Self {
        if self.renderer.supports_apply_config() {
            self.renderer.apply_config(config);
        } else {
            self.platform.warn(&format!(
                "[vela] renderer \"{}\" does not support applyConfig().",
                self.renderer.name()
            ));
        }
        self
    }

    /// Subscribe to cosmetic-config changes (`apply_config`: the in-chart
    /// settings dialog commits through it). Host chrome that mirrors a config
    /// value (a bottom-bar timezone) re-pulls [`get`](Self::get) /
    /// [`get_config`](Self::get_config) here. A no-op unsubscribe on a renderer
    /// without a rich config.
    pub fn on_config_changed(&self, cb: Box<dyn FnMut()>) -> Unsubscribe {
        self.renderer
            .on_config_changed(cb)
            .unwrap_or_else(noop_unsubscribe)
    }

    /// Subscribe to crosshair movement: `time`/`price` under the cursor,
    /// per-series values, and the hovered bar's OHLC (empty fields when the
    /// cursor leaves the chart). This is the public seam host chrome (status
    /// lines, data windows) builds on.
    pub fn on_crosshair_move(&self, cb: Listener<CrosshairEvent>) -> Unsubscribe {
        self.renderer.on_crosshair_move(cb)
    }

    /// A click (or a touch tap) on the plot, never the end of a pan: the open
    /// time of the bar under it (`None` off the bars). On touch, where a tap moves
    /// no crosshair, this is how an interaction learns which bar was chosen.
    pub fn on_click(&self, cb: Listener<ClickEvent>) -> Unsubscribe {
        self.renderer.on_click(cb)
    }

    /// Touch long-press on a price or time axis strip: a no-op unsubscribe
    /// without the seam.
    pub fn on_axis_long_press(&self, cb: Listener<AxisLongPressEvent>) -> Unsubscribe {
        self.renderer
            .on_axis_long_press(cb)
            .unwrap_or_else(noop_unsubscribe)
    }

    /// The current data-window readout: the hovered bar's date/time and OHLCV
    /// plus every indicator's value there, each already formatted on its pane's
    /// scale (the latest bar when the cursor is off the plot). Pair it with
    /// [`on_crosshair_move`](Self::on_crosshair_move) to drive a data-window
    /// panel. `None` on a renderer that does not provide the readout.
    pub fn data_window_readout(&self) -> Option<DataWindowReadout> {
        self.renderer.get_data_window_readout()
    }

    /// Wire (or clear with `None`) the host's symbol picker for the settings
    /// dialog's `input.symbol` control: the host opens its own ticker-selection UI
    /// and reports the chosen symbol back. Does nothing, with a warning, if the
    /// active renderer does not support it (only the native renderer does).
    pub fn set_symbol_picker(&self, picker: Option<SymbolPickerFn>) -> &Self {
        if self.renderer.supports_set_symbol_picker() {
            self.renderer.set_symbol_picker(picker);
        } else {
            self.platform.warn(&format!(
                "[vela] renderer \"{}\" does not support setSymbolPicker().",
                self.renderer.name()
            ));
        }
        self
    }

    /// Move keyboard focus back onto the chart's interactive surface: call after
    /// a host control (a shared toolbar button) stole focus, so chart and drawing
    /// shortcuts keep working. Silent no-op on a renderer without a focusable
    /// surface.
    pub fn focus(&self) -> &Self {
        self.renderer.focus();
        self
    }

    /// Whether the active renderer can display an EXTERNAL (synced) crosshair.
    pub fn supports_external_crosshair(&self) -> bool {
        self.renderer.supports_set_external_crosshair()
    }

    /// Show (or clear, with `None`) a data-space ghost crosshair driven from
    /// OUTSIDE this chart: the multi-chart crosshair-sync seam. Silent no-op on a
    /// renderer without the capability (see
    /// [`supports_external_crosshair`](Self::supports_external_crosshair)).
    pub fn set_external_crosshair(&self, time: Option<Millis>, price: Option<f64>) -> &Self {
        self.renderer.set_external_crosshair(time, price);
        self
    }

    /// Close any in-chart dialogs the active renderer owns (indicator settings,
    /// chart-settings gear): for keeping host dialogs mutually exclusive with the
    /// renderer's. Silent no-op if the renderer has no such dialogs; safe to call
    /// speculatively.
    pub fn close_dialogs(&self) -> &Self {
        self.renderer.close_dialogs();
        self
    }

    /// Open (or toggle) the renderer's in-chart settings dialog: a silent no-op
    /// without one. Pass a section title (e.g. `Canvas`) to land on that tab; an
    /// unknown one opens the dialog on its first tab, and with a section an open
    /// dialog switches tab instead of closing.
    pub fn open_settings(&self, section: Option<&str>) -> &Self {
        self.renderer.open_settings_dialog(section);
        self
    }

    /// Contribute host settings tabs (callback rows) to the renderer's settings
    /// dialog, e.g. the widget's Status line toggles. Silent no-op without a dialog.
    pub fn set_settings_sections(&self, sections: &[SettingsSection]) -> &Self {
        self.renderer.set_settings_sections(sections);
        self
    }

    /// Set the settings-dialog visibility policy: `hidden` lists setting ids to
    /// hide: a tab (`canvas`), a group (`canvas.grid`), or a single row
    /// (`canvas.grid.vertical`); an id hides its whole subtree, and a tab with
    /// nothing left disappears from the rail. Presentation-only: hidden values
    /// keep being stored and applied (e.g. hide `advanced` while forcing the
    /// widget's `bars` option). Seeded from `VelaOptions::settings`; silent no-op
    /// without a settings dialog.
    pub fn set_settings_visibility(&self, policy: &SettingsVisibility) -> &Self {
        self.renderer.set_settings_visibility(policy);
        self
    }

    /// Every addressable setting id of this chart (built-in tabs, groups and rows,
    /// chart-type sections, host sections): enumerate these to build a `hidden`
    /// list instead of reading contributor source. Empty on a renderer without a
    /// settings dialog.
    pub fn list_settings_ids(&self) -> Vec<String> {
        self.renderer.list_settings_ids()
    }

    /// Tell the renderer's own chrome which size class the host shell is in:
    /// `Mobile` switches its dialogs and toolbars to the touch-first
    /// presentation. Silent no-op on a renderer without adaptive chrome.
    pub fn set_layout_mode(&self, mode: LayoutMode) -> &Self {
        self.renderer.set_layout_mode(mode);
        self
    }

    /// Drive the renderer's time-of-day chrome (the countdown-to-bar-close chip)
    /// from the host's own second pulse, so it ticks in step with a host clock
    /// display instead of on a separate timer that can read a different second.
    /// `None` hands the pulse back to the renderer. Silent no-op on a renderer
    /// without time-of-day chrome.
    pub fn set_wall_clock(&self, clock: Option<Rc<dyn WallClock>>) -> &Self {
        self.renderer.set_wall_clock(clock);
        self
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use serde_json::json;

    use super::*;
    use crate::model::{IndicatorModel, Inputs, Pane, ScenePatch};
    use crate::ports::chart_renderer::{
        BarColorSupport, ClickEvent, IndicatorRenderHandle, InputChangeEvent, SetBarsOptions,
        Support, VelaTheme, VisibleRange, VisibleRangeOptions,
    };
    use crate::runtime::testing::ManualPlatform;

    struct Fake {
        features: Vec<String>,
        rich: bool,
        log: RefCell<Vec<String>>,
        applied: RefCell<Vec<(String, Value)>>,
        config_listeners: Cell<usize>,
    }

    impl Fake {
        fn new(rich: bool) -> Rc<Self> {
            Rc::new(Self {
                features: vec!["glow".into(), "logScale".into()],
                rich,
                log: RefCell::default(),
                applied: RefCell::default(),
                config_listeners: Cell::new(0),
            })
        }

        fn note(&self, line: impl Into<String>) {
            self.log.borrow_mut().push(line.into());
        }
    }

    impl ChartRenderer for Fake {
        fn capabilities(&self) -> RendererCapabilities {
            RendererCapabilities {
                panes: true,
                pane_management: true,
                fills: Support::Native,
                bgcolor: Support::Native,
                hline: Support::Native,
                markers: true,
                barcolor: BarColorSupport::Native,
                per_point_color: true,
                drawings: true,
                user_drawings: false,
                drawing_depth: false,
                tables: true,
                trades: false,
                timeline_marks: false,
                inputs_ui: true,
            }
        }
        fn name(&self) -> &str {
            "fake"
        }
        fn features(&self) -> Vec<String> {
            self.features.clone()
        }
        fn apply_feature(&self, key: &str, value: &Value) {
            self.applied
                .borrow_mut()
                .push((key.to_string(), value.clone()));
        }
        fn read_feature(&self, key: &str) -> Option<Value> {
            self.features
                .iter()
                .any(|f| f == key)
                .then(|| json!(key.len()))
        }
        fn mount(&self, _theme: &VelaTheme) {}
        fn set_theme(&self, _theme: &VelaTheme) {}
        fn resize(&self) {}
        fn destroy(&self) {}
        fn set_bars(&self, _bars: &[crate::model::Ohlcv], _opts: SetBarsOptions) {}
        fn update_bar(&self, _bar: &crate::model::Ohlcv) {}
        fn ensure_pane(&self, _pane: &Pane) {}
        fn remove_pane(&self, _id: &str) {}
        fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
            IndicatorRenderHandle {
                id: model.id.clone(),
            }
        }
        fn update_indicator(&self, _h: &IndicatorRenderHandle, _p: &ScenePatch) {}
        fn remove_indicator(&self, _h: &IndicatorRenderHandle) {}
        fn set_indicator_inputs(
            &self,
            _h: &IndicatorRenderHandle,
            _v: &Inputs,
            _p: Option<&Inputs>,
        ) {
        }
        fn on_input_change(&self, _cb: Listener<InputChangeEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_remove_indicator(&self, _cb: Listener<String>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_crosshair_move(&self, _cb: Listener<CrosshairEvent>) -> Unsubscribe {
            self.note("onCrosshairMove");
            noop_unsubscribe()
        }
        fn on_click(&self, _cb: Listener<ClickEvent>) -> Unsubscribe {
            self.note("onClick");
            noop_unsubscribe()
        }
        fn get_visible_range(&self) -> Option<VisibleRange> {
            None
        }
        fn set_visible_range(&self, _r: VisibleRange, _o: VisibleRangeOptions) {}
        fn on_viewport_change(&self, _cb: Listener<VisibleRange>) -> Unsubscribe {
            noop_unsubscribe()
        }

        // Optional members, supported only by a "rich" fake.
        fn set_legend_actions(&self, provider: Option<LegendActionProvider>) {
            self.note(format!("legendActions {}", provider.is_some()));
        }
        fn set_legend_callouts(&self, provider: Option<LegendCalloutProvider>) {
            self.note(format!("legendCallouts {}", provider.is_some()));
        }
        fn set_legend_overview_action(&self, action: Option<Rc<dyn Fn()>>) {
            self.note(format!("overview {}", action.is_some()));
        }
        fn open_indicator_settings(&self, id: &str) {
            self.note(format!("openIndicatorSettings {id}"));
        }
        fn supports_open_indicator_settings(&self) -> bool {
            self.rich
        }
        fn screenshot(&self) -> Option<String> {
            self.rich.then(|| "data:image/png;base64,AA".to_string())
        }
        fn supports_screenshot(&self) -> bool {
            self.rich
        }
        fn get_config(&self) -> Option<Value> {
            self.rich.then(|| json!({ "version": 1 }))
        }
        fn apply_config(&self, config: &Value) {
            self.note(format!("applyConfig {config}"));
        }
        fn supports_apply_config(&self) -> bool {
            self.rich
        }
        fn on_config_changed(&self, _cb: Box<dyn FnMut()>) -> Option<Unsubscribe> {
            self.rich.then(|| {
                self.config_listeners.set(self.config_listeners.get() + 1);
                noop_unsubscribe()
            })
        }
        fn on_axis_long_press(&self, _cb: Listener<AxisLongPressEvent>) -> Option<Unsubscribe> {
            self.rich.then(noop_unsubscribe)
        }
        fn get_data_window_readout(&self) -> Option<DataWindowReadout> {
            self.rich.then(|| DataWindowReadout {
                date: "2026-07-03".into(),
                time: "10:00".into(),
                ohlc: None,
                groups: Vec::new(),
            })
        }
        fn set_symbol_picker(&self, picker: Option<SymbolPickerFn>) {
            self.note(format!("symbolPicker {}", picker.is_some()));
        }
        fn supports_set_symbol_picker(&self) -> bool {
            self.rich
        }
        fn focus(&self) {
            self.note("focus");
        }
        fn set_external_crosshair(&self, time: Option<Millis>, price: Option<f64>) {
            self.note(format!("externalCrosshair {time:?} {price:?}"));
        }
        fn supports_set_external_crosshair(&self) -> bool {
            self.rich
        }
        fn close_dialogs(&self) {
            self.note("closeDialogs");
        }
        fn open_settings_dialog(&self, section: Option<&str>) {
            self.note(format!("openSettings {section:?}"));
        }
        fn set_settings_sections(&self, sections: &[SettingsSection]) {
            self.note(format!("sections {}", sections.len()));
        }
        fn set_settings_visibility(&self, policy: &SettingsVisibility) {
            self.note(format!("visibility {:?}", policy.hidden));
        }
        fn list_settings_ids(&self) -> Vec<String> {
            if self.rich {
                vec!["canvas".into(), "canvas.grid".into()]
            } else {
                Vec::new()
            }
        }
        fn set_layout_mode(&self, mode: LayoutMode) {
            self.note(format!("layout {mode:?}"));
        }
        fn set_wall_clock(&self, clock: Option<Rc<dyn WallClock>>) {
            self.note(format!("wallClock {}", clock.is_some()));
        }
    }

    fn rig(rich: bool) -> (ManualPlatform, Rc<Fake>, RendererControl) {
        let platform = ManualPlatform::new(0.0);
        let fake = Fake::new(rich);
        let control = RendererControl::new(fake.clone(), platform.handle());
        (platform, fake, control)
    }

    #[test]
    fn identity_and_features() {
        let (_, _, control) = rig(true);
        assert_eq!(control.name(), "fake");
        assert!(control.capabilities().panes);
        assert!(control.supports("glow"));
        assert!(!control.supports("nope"));
        assert_eq!(control.get("glow"), Some(json!(4)));
        assert_eq!(control.get("nope"), None);
    }

    #[test]
    fn set_applies_supported_features_and_warns_about_the_rest() {
        let (platform, fake, control) = rig(true);
        control
            .set("glow", json!(0.6))
            .set("bogus", json!(1))
            .set("logScale", json!(true));
        assert_eq!(
            *fake.applied.borrow(),
            [
                ("glow".to_string(), json!(0.6)),
                ("logScale".to_string(), json!(true))
            ]
        );
        assert_eq!(
            platform.warnings(),
            ["[vela] renderer \"fake\" has no feature \"bogus\" \u{2014} set ignored."]
        );
    }

    #[test]
    fn set_many_applies_in_order_and_warns_per_unknown_key() {
        let (platform, fake, control) = rig(true);
        control.set_many([
            ("logScale".to_string(), json!(true)),
            ("a".to_string(), json!(1)),
            ("glow".to_string(), json!(0.1)),
            ("b".to_string(), json!(2)),
        ]);
        let keys: Vec<String> = fake
            .applied
            .borrow()
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(keys, ["logScale", "glow"]);
        assert_eq!(platform.warnings().len(), 2);
    }

    #[test]
    fn optional_seams_forward_when_supported() {
        let (platform, fake, control) = rig(true);
        assert!(control.supports_indicator_settings());
        assert!(control.supports_external_crosshair());
        control
            .open_indicator_settings("ind-1")
            .set_legend_overview_action(Some(Rc::new(|| {})))
            .set_external_crosshair(Some(5), Some(1.5))
            .set_external_crosshair(None, None)
            .focus()
            .close_dialogs()
            .open_settings(Some("Canvas"))
            .open_settings(None)
            .set_settings_sections(&[])
            .set_settings_visibility(&SettingsVisibility {
                hidden: Some(vec!["canvas.grid".into()]),
            })
            .set_layout_mode(LayoutMode::Mobile)
            .set_wall_clock(None)
            .set_symbol_picker(Some(Rc::new(|_, _| {})))
            .apply_config(&json!({ "a": 1 }));
        control.set_legend_actions(Some(Rc::new(|_| Vec::new())));
        control.set_legend_callouts(None);
        assert_eq!(
            *fake.log.borrow(),
            [
                "openIndicatorSettings ind-1",
                "overview true",
                "externalCrosshair Some(5) Some(1.5)",
                "externalCrosshair None None",
                "focus",
                "closeDialogs",
                "openSettings Some(\"Canvas\")",
                "openSettings None",
                "sections 0",
                "visibility Some([\"canvas.grid\"])",
                "layout Mobile",
                "wallClock false",
                "symbolPicker true",
                "applyConfig {\"a\":1}",
                "legendActions true",
                "legendCallouts false",
            ]
        );
        assert!(platform.warnings().is_empty());
        assert_eq!(control.list_settings_ids(), ["canvas", "canvas.grid"]);
        assert!(control.data_window_readout().is_some());
        assert_eq!(
            control.screenshot().as_deref(),
            Some("data:image/png;base64,AA")
        );
        assert_eq!(control.get_config(), Some(json!({ "version": 1 })));
        control.on_config_changed(Box::new(|| {}))();
        control.on_axis_long_press(Box::new(|_| {}))();
        control.on_crosshair_move(Box::new(|_| {}))();
        control.on_click(Box::new(|_| {}))();
        assert_eq!(fake.config_listeners.get(), 1);
        assert!(
            fake.log
                .borrow()
                .ends_with(&["onCrosshairMove".to_string(), "onClick".to_string()])
        );
    }

    #[test]
    fn a_renderer_without_the_seams_warns_where_the_original_warns_and_is_silent_elsewhere() {
        let (platform, fake, control) = rig(false);
        assert!(!control.supports_indicator_settings());
        assert!(!control.supports_external_crosshair());
        assert_eq!(control.screenshot(), None);
        assert_eq!(control.get_config(), None);
        control.apply_config(&json!({}));
        control.set_symbol_picker(None);
        assert_eq!(
            platform.warnings(),
            [
                "[vela] renderer \"fake\" does not support screenshot().",
                "[vela] renderer \"fake\" does not support getConfig().",
                "[vela] renderer \"fake\" does not support applyConfig().",
                "[vela] renderer \"fake\" does not support setSymbolPicker().",
            ]
        );
        // The silent ones still reach the renderer, which ignores them.
        assert!(control.data_window_readout().is_none());
        assert!(control.list_settings_ids().is_empty());
        control.on_config_changed(Box::new(|| {}))();
        control.on_axis_long_press(Box::new(|_| {}))();
        assert_eq!(fake.config_listeners.get(), 0);
        assert!(fake.log.borrow().is_empty());
        assert_eq!(platform.warnings().len(), 4);
    }
}
