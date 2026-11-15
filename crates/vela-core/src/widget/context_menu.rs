//! Chart context menu: right-click on the chart area. Items depend on the zone (price axis /
//! time axis / chart body) and the current renderer feature states; the rows themselves are
//! built by [`context_menu_model`](super::context_menu_model).
//!
//! The pure half of the original `ChartContextMenu`: classifying the right-clicked zone
//! ([`zone_of`]), capturing the crosshair as the menu opens ([`ContextMenuState`]), the
//! context handed to contributed actions ([`PointerContext`]), the contributed rows
//! ([`contributed_rows`]) and routing a selected row to a command ([`route_selection`]).
//! Left for the browser crate: the kit `ContextMenu` opened at the pointer (no long press:
//! it is the chart's touch crosshair), the `contextmenu` listener (which also prevents the
//! browser's own menu), reading the renderer's features to feed the model builders, and
//! applying a [`MenuCommand`] to the chart and its renderer.

use std::rc::Rc;

use super::context_menu_model::{
    ContributedRow, PaneScaleInfo, ScaleChoice, Zone, pane_scale_at, settings_section_of,
};
use super::contributions::{
    ContextMenuPointer, Contributions, DockEdge, DockStripOptions, DockedStrip, ElementHandle,
    ExternalIndicatorEntry, PanelDockView, PointerPaneKind, SidebarControl, ToastKind, TopbarItems,
    WidgetActionTarget, WidgetContext,
};
use crate::i18n::{Translator, text_of};
use crate::runtime::Unsubscribe;
use crate::ui::MenuItemDescriptor;
use crate::vela::Vela;

/// Approximate chrome insets used only to classify the right-clicked zone.
pub const PRICE_AXIS_W: f64 = 60.0;
pub const TIME_AXIS_H: f64 = 26.0;

/// What the pointer reports off the plot.
pub fn off_plot() -> ContextMenuPointer {
    ContextMenuPointer {
        price: None,
        time: None,
        pane_kind: None,
        pane_id: None,
    }
}

/// The right-clicked zone: the price axis along the right edge, the time axis along the
/// bottom, else the body. Coordinates are viewport; `rect` is the host's box.
pub fn zone_of(client_x: f64, client_y: f64, left: f64, top: f64, width: f64, height: f64) -> Zone {
    if client_x - left > width - PRICE_AXIS_W {
        Zone::PriceAxis
    } else if client_y - top > height - TIME_AXIS_H {
        Zone::TimeAxis
    } else {
        Zone::Body
    }
}

/// The action-target of a zone's contributed rows.
pub fn target_of(zone: Zone) -> WidgetActionTarget {
    match zone {
        Zone::Body => WidgetActionTarget::ContextBody,
        Zone::PriceAxis => WidgetActionTarget::ContextPriceAxis,
        Zone::TimeAxis => WidgetActionTarget::ContextTimeAxis,
    }
}

/// The state of one menu: the bound chart's latest crosshair report and what was captured
/// when the menu opened.
#[derive(Clone, Debug)]
pub struct ContextMenuState {
    /// The bound chart's latest crosshair report: a right-click always follows a pointer
    /// move to its spot, so this is where it landed.
    crosshair: ContextMenuPointer,
    /// `crosshair` as it stood when the menu opened: the pointer moves on through the menu
    /// before a row is picked, and `run` must see the right-clicked spot.
    pointer: ContextMenuPointer,
    zone: Zone,
    /// The pane whose scale the open price-axis menu targets (`None`: the main scale).
    pane: Option<PaneScaleInfo>,
}

impl Default for ContextMenuState {
    fn default() -> Self {
        Self {
            crosshair: off_plot(),
            pointer: off_plot(),
            zone: Zone::Body,
            pane: None,
        }
    }
}

impl ContextMenuState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebinding to a chart forgets the previous one's crosshair.
    pub fn rebind(&mut self) {
        self.crosshair = off_plot();
    }

    /// A crosshair report from the renderer. A renderer that reports no pane id leaves it
    /// `None`.
    pub fn on_crosshair(
        &mut self,
        price: Option<f64>,
        time: Option<f64>,
        pane_kind: Option<PointerPaneKind>,
        pane_id: Option<String>,
    ) {
        self.crosshair = ContextMenuPointer {
            price,
            time,
            pane_kind,
            pane_id,
        };
    }

    /// The menu opens at `zone`. `panes` is the renderer's `paneScales` (a price-axis menu
    /// targets the pane under `local_y`, plot-local).
    pub fn open(&mut self, zone: Zone, panes: Option<&[PaneScaleInfo]>, local_y: f64) {
        self.pointer = self.crosshair.clone();
        self.zone = zone;
        self.pane = if zone == Zone::PriceAxis {
            panes.and_then(|p| pane_scale_at(p, local_y)).cloned()
        } else {
            None
        };
    }

    pub fn zone(&self) -> Zone {
        self.zone
    }

    pub fn pane(&self) -> Option<&PaneScaleInfo> {
        self.pane.as_ref()
    }

    /// The pointer captured at open.
    pub fn pointer(&self) -> &ContextMenuPointer {
        &self.pointer
    }
}

/// The widget context carrying the pointer captured at open. Everything else delegates to the
/// shell's own context, so its getters stay LIVE (the original copies property descriptors for
/// the same reason).
pub struct PointerContext<'a> {
    inner: &'a dyn WidgetContext,
    pointer: ContextMenuPointer,
}

impl<'a> PointerContext<'a> {
    pub fn new(inner: &'a dyn WidgetContext, pointer: ContextMenuPointer) -> Self {
        Self { inner, pointer }
    }
}

impl WidgetContext for PointerContext<'_> {
    fn chart(&self) -> Vela {
        self.inner.chart()
    }
    fn symbol(&self) -> String {
        self.inner.symbol()
    }
    fn timeframe(&self) -> String {
        self.inner.timeframe()
    }
    fn price_style(&self) -> String {
        self.inner.price_style()
    }
    fn set_symbol(&self, symbol: &str) {
        self.inner.set_symbol(symbol);
    }
    fn set_timeframe(&self, tf: &str) {
        self.inner.set_timeframe(tf);
    }
    fn set_price_style(&self, style: &str) {
        self.inner.set_price_style(style);
    }
    fn open_symbol_search(&self, query: Option<&str>) {
        self.inner.open_symbol_search(query);
    }
    fn toggle_panel(&self, id: &str, open: Option<bool>) {
        self.inner.toggle_panel(id, open);
    }
    fn dock_strip(&self, el: ElementHandle, opts: DockStripOptions) -> Box<dyn DockedStrip> {
        self.inner.dock_strip(el, opts)
    }
    fn dock_edge(&self, el: ElementHandle, side: DockEdge) -> Unsubscribe {
        self.inner.dock_edge(el, side)
    }
    fn host(&self) -> ElementHandle {
        self.inner.host()
    }
    fn i18n(&self) -> Rc<dyn Translator> {
        self.inner.i18n()
    }
    fn toast(&self, message: &str, kind: ToastKind) {
        self.inner.toast(message, kind);
    }
    fn add_indicator(&self, entry: ExternalIndicatorEntry) {
        self.inner.add_indicator(entry);
    }
    fn add_native_indicator(&self, indicator_type: &str) {
        self.inner.add_native_indicator(indicator_type);
    }
    fn state_changed(&self) {
        self.inner.state_changed();
    }
    fn topbar(&self) -> &dyn TopbarItems {
        self.inner.topbar()
    }
    fn panels(&self) -> &dyn PanelDockView {
        self.inner.panels()
    }
    fn sidebar(&self) -> &dyn SidebarControl {
        self.inner.sidebar()
    }
    fn pointer(&self) -> Option<ContextMenuPointer> {
        Some(self.pointer.clone())
    }
}

/// The contributed rows of a zone, `when`-gated by a context carrying the open-time pointer.
/// A contributed row takes its zone's look: `iconic` is whether the built-in rows carry icons
/// (one icon in an icon-free menu, the axis menus with their checkmarks, would push every
/// label right of an empty icon column).
pub fn contributed_rows(
    contributions: &Contributions,
    ctx: &dyn WidgetContext,
    zone: Zone,
    iconic: bool,
    i18n: Option<&dyn Translator>,
) -> Vec<ContributedRow> {
    contributions
        .widget_actions(target_of(zone), Some(ctx))
        .into_iter()
        .map(|a| ContributedRow {
            item: MenuItemDescriptor {
                icon: if iconic { a.icon.clone() } else { None },
                ..MenuItemDescriptor::new(format!("action:{}", a.id), text_of(i18n, &a.label, &[]))
            },
            order: a.order,
        })
        .collect()
}

/// What a selected row asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuCommand {
    /// Run the contributed action with this id (`action:<id>` without the prefix).
    RunAction(String),
    /// Flip a boolean renderer feature (`toggle:<feature>`).
    ToggleFeature(String),
    /// Pick a price-scale choice.
    Scale(ScaleChoice),
    /// Switch the display timezone (a zone, or the exchange rule).
    Timezone(String),
    /// Flip `autoScale` against its own default.
    AutoScale,
    /// Flip `scalePriceOnly`.
    PriceOnly,
    /// Invert the targeted pane's axis.
    Invert,
    /// Open the settings dialog on a tab (`None`: its first).
    Settings(Option<String>),
    ResetView,
    RemoveDrawings,
    RemoveIndicators,
}

/// Route a selected row id. `None` for an id the menu does not act on.
pub fn route_selection(id: &str) -> Option<MenuCommand> {
    if let Some(action) = id.strip_prefix("action:") {
        return Some(MenuCommand::RunAction(action.to_string()));
    }
    if let Some(feature) = id.strip_prefix("toggle:") {
        return Some(MenuCommand::ToggleFeature(feature.to_string()));
    }
    if let Some(choice) = id.strip_prefix("scale:") {
        let choice = match choice {
            "regular" => ScaleChoice::Regular,
            "percent" => ScaleChoice::Percent,
            "indexed" => ScaleChoice::Indexed,
            "log" => ScaleChoice::Log,
            _ => return None,
        };
        return Some(MenuCommand::Scale(choice));
    }
    if let Some(zone) = id.strip_prefix("tz:") {
        return Some(MenuCommand::Timezone(zone.to_string()));
    }
    if id.starts_with("settings") {
        return Some(MenuCommand::Settings(settings_section_of(id)));
    }
    match id {
        "auto" => Some(MenuCommand::AutoScale),
        "price-only" => Some(MenuCommand::PriceOnly),
        "invert" => Some(MenuCommand::Invert),
        "reset-view" => Some(MenuCommand::ResetView),
        "remove-drawings" => Some(MenuCommand::RemoveDrawings),
        "remove-indicators" => Some(MenuCommand::RemoveIndicators),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use crate::widget::context_menu_model::{
        PaneKind, PriceAxisState, body_items, compose_menu, price_axis_items,
    };
    use crate::widget::contributions::WidgetActionDescriptor;
    use crate::widget::test_support::ctx;
    use std::cell::RefCell;

    fn contributions() -> Contributions {
        Contributions::new(ManualPlatform::new(0.0).handle())
    }

    #[test]
    fn the_zone_follows_the_edge_the_click_landed_on() {
        // An 800x600 host at the origin.
        assert_eq!(
            zone_of(795.0, 200.0, 0.0, 0.0, 800.0, 600.0),
            Zone::PriceAxis
        );
        assert_eq!(
            zone_of(300.0, 590.0, 0.0, 0.0, 800.0, 600.0),
            Zone::TimeAxis
        );
        assert_eq!(zone_of(300.0, 200.0, 0.0, 0.0, 800.0, 600.0), Zone::Body);
        // The price axis wins the corner; the host's own offset is subtracted.
        assert_eq!(
            zone_of(795.0, 595.0, 0.0, 0.0, 800.0, 600.0),
            Zone::PriceAxis
        );
        assert_eq!(
            zone_of(1095.0, 300.0, 300.0, 100.0, 800.0, 600.0),
            Zone::PriceAxis
        );
        assert_eq!(zone_of(740.0, 200.0, 0.0, 0.0, 800.0, 600.0), Zone::Body);
    }

    #[test]
    fn the_menu_hands_when_and_run_the_crosshair_captured_when_it_opened() {
        let mut state = ContextMenuState::new();
        state.on_crosshair(
            Some(42_123.5),
            Some(1_700_000_000_000.0),
            Some(PointerPaneKind::Price),
            Some("price".into()),
        );
        state.open(Zone::Body, None, 200.0);
        let expected = ContextMenuPointer {
            price: Some(42_123.5),
            time: Some(1_700_000_000_000.0),
            pane_kind: Some(PointerPaneKind::Price),
            pane_id: Some("price".into()),
        };
        assert_eq!(state.pointer(), &expected);
        // The pointer travels through the menu (and off the chart) before the row is picked.
        state.on_crosshair(
            Some(41_000.0),
            Some(1_700_000_060_000.0),
            Some(PointerPaneKind::Price),
            None,
        );
        state.on_crosshair(None, None, None, None);
        assert_eq!(state.pointer(), &expected);
    }

    #[test]
    fn a_study_pane_reports_by_kind_and_id_and_off_the_plot_everything_is_none() {
        let mut state = ContextMenuState::new();
        state.on_crosshair(
            Some(61.2),
            Some(5.0),
            Some(PointerPaneKind::Study),
            Some("pane-rsi".into()),
        );
        state.open(Zone::Body, None, 500.0);
        assert_eq!(state.pointer().pane_kind, Some(PointerPaneKind::Study));
        assert_eq!(state.pointer().pane_id.as_deref(), Some("pane-rsi"));
        state.on_crosshair(None, None, None, None);
        state.open(Zone::Body, None, 200.0);
        assert_eq!(state.pointer(), &off_plot());
        // A fresh state, and a rebound one, start off the plot.
        assert_eq!(ContextMenuState::new().pointer(), &off_plot());
        state.on_crosshair(Some(1.0), Some(2.0), None, None);
        state.rebind();
        state.open(Zone::Body, None, 0.0);
        assert_eq!(state.pointer(), &off_plot());
    }

    #[test]
    fn a_price_axis_menu_targets_the_pane_under_the_pointer() {
        let panes = vec![
            PaneScaleInfo {
                id: "price".into(),
                kind: PaneKind::Price,
                top: 0.0,
                height: 300.0,
                mode: "price".into(),
                log: false,
                invert: false,
            },
            PaneScaleInfo {
                id: "p2".into(),
                kind: PaneKind::Study,
                top: 300.0,
                height: 100.0,
                mode: "price".into(),
                log: false,
                invert: true,
            },
        ];
        let mut state = ContextMenuState::new();
        state.open(Zone::PriceAxis, Some(&panes), 350.0);
        assert_eq!(state.pane().map(|p| p.id.as_str()), Some("p2"));
        assert_eq!(state.zone(), Zone::PriceAxis);
        // Another zone, or a renderer without per-pane scales, targets the main scale.
        state.open(Zone::Body, Some(&panes), 350.0);
        assert!(state.pane().is_none());
        state.open(Zone::PriceAxis, None, 350.0);
        assert!(state.pane().is_none());
    }

    #[test]
    fn the_pointer_context_adds_the_pointer_and_keeps_the_live_getters() {
        let shell = ctx(1);
        assert_eq!(WidgetContext::pointer(&shell), None);
        let at = ContextMenuPointer {
            price: Some(1.0),
            time: Some(2.0),
            pane_kind: None,
            pane_id: None,
        };
        let wrapped = PointerContext::new(&shell, at.clone());
        assert_eq!(wrapped.pointer(), Some(at));
        assert_eq!(wrapped.symbol(), "BTCUSDT");
        *shell.symbol.borrow_mut() = "ETHUSDT".into();
        assert_eq!(
            wrapped.symbol(),
            "ETHUSDT",
            "the context's getters stay LIVE"
        );
        // The shell's own context is left as it was.
        assert_eq!(WidgetContext::pointer(&shell), None);
    }

    fn copy_price(seen: Rc<RefCell<Vec<Option<ContextMenuPointer>>>>) -> WidgetActionDescriptor {
        let mut a = WidgetActionDescriptor::new(
            "test.copy-price",
            WidgetActionTarget::ContextBody,
            "Copy price",
            |_| {},
        );
        a.icon = Some("clone".into());
        a.order = Some(-100.0);
        a.when = Some(Rc::new(move |ctx| {
            seen.borrow_mut().push(ctx.pointer());
            true
        }));
        a
    }

    #[test]
    fn contributed_rows_are_gated_with_the_pointer_and_list_first_at_order_minus_100() {
        let c = contributions();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let _ = c.register_widget_action(copy_price(Rc::clone(&seen)));
        let mut state = ContextMenuState::new();
        state.on_crosshair(Some(2.0), Some(1.0), Some(PointerPaneKind::Price), None);
        state.open(Zone::Body, None, 200.0);
        let shell = ctx(1);
        let with_pointer = PointerContext::new(&shell, state.pointer().clone());
        let rows = contributed_rows(&c, &with_pointer, Zone::Body, true, None);
        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(seen.borrow()[0].as_ref().and_then(|p| p.price), Some(2.0));
        let items = compose_menu(Zone::Body, &body_items(1, 1, None), &rows);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "action:test.copy-price",
                "reset-view",
                "remove-drawings",
                "remove-indicators",
                "settings:Canvas"
            ]
        );
        assert_eq!(items[0].label, "Copy price");
        assert_eq!(items[0].icon.as_deref(), Some("clone"));
        assert!(!items[0].separator_before && items[1].separator_before);
    }

    #[test]
    fn a_contributed_row_wears_its_icon_in_the_body_menu_and_not_on_the_axis_menus() {
        let c = contributions();
        let mut alert = WidgetActionDescriptor::new(
            "test.alert",
            WidgetActionTarget::ContextPriceAxis,
            "Add alert",
            |_| {},
        );
        alert.icon = Some("alert".into());
        alert.order = Some(-90.0);
        let _ = c.register_widget_action(alert);
        let shell = ctx(1);
        let axis_state = PriceAxisState {
            auto: true,
            price_only: None,
            invert: false,
            choice: ScaleChoice::Regular,
            axis_labels: true,
            price_label: true,
            countdown: false,
            price_line: true,
        };
        let builtin = price_axis_items(&axis_state, None);
        let iconic = builtin.iter().any(|i| i.icon.is_some());
        assert!(!iconic);
        let rows = contributed_rows(&c, &shell, Zone::PriceAxis, iconic, None);
        let menu = compose_menu(Zone::PriceAxis, &builtin, &rows);
        let row = menu
            .iter()
            .find(|i| i.id == "action:test.alert")
            .expect("the row");
        assert!(row.icon.is_none());
        assert!(menu.iter().all(|i| i.icon.is_none()));
        // The same action in an iconic menu keeps its icon.
        let again = contributed_rows(&c, &shell, Zone::PriceAxis, true, None);
        assert_eq!(again[0].item.icon.as_deref(), Some("alert"));
    }

    #[test]
    fn selections_route_to_commands() {
        assert_eq!(
            route_selection("action:x.y"),
            Some(MenuCommand::RunAction("x.y".into()))
        );
        assert_eq!(
            route_selection("toggle:countdown"),
            Some(MenuCommand::ToggleFeature("countdown".into()))
        );
        assert_eq!(
            route_selection("scale:indexed"),
            Some(MenuCommand::Scale(ScaleChoice::Indexed))
        );
        assert_eq!(route_selection("scale:nope"), None);
        assert_eq!(
            route_selection("tz:Europe/Paris"),
            Some(MenuCommand::Timezone("Europe/Paris".into()))
        );
        assert_eq!(
            route_selection("tz:exchange"),
            Some(MenuCommand::Timezone("exchange".into()))
        );
        assert_eq!(route_selection("auto"), Some(MenuCommand::AutoScale));
        assert_eq!(route_selection("price-only"), Some(MenuCommand::PriceOnly));
        assert_eq!(route_selection("invert"), Some(MenuCommand::Invert));
        assert_eq!(
            route_selection("settings:Canvas"),
            Some(MenuCommand::Settings(Some("Canvas".into())))
        );
        assert_eq!(
            route_selection("settings"),
            Some(MenuCommand::Settings(None))
        );
        assert_eq!(route_selection("reset-view"), Some(MenuCommand::ResetView));
        assert_eq!(
            route_selection("remove-drawings"),
            Some(MenuCommand::RemoveDrawings)
        );
        assert_eq!(
            route_selection("remove-indicators"),
            Some(MenuCommand::RemoveIndicators)
        );
        assert_eq!(route_selection("labels"), None);
        assert_eq!(route_selection(""), None);
    }
}
