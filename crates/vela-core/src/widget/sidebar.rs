//! The shell's right-edge toolbar (the `sidebar` option): the side-panel toggles and the
//! actions registered with target `sidebar`, laid out by the host's
//! [`SidebarComposition`](super::sidebar_composition::SidebarComposition).
//!
//! The pure half of the original `Sidebar`: [`SidebarState`] keeps the panel buttons, the
//! pressed panels, the badges and the entries a plugin hid at runtime, and lays the entries
//! out ([`SidebarState::layout`]). [`SidebarHandle`] is the `ctx.sidebar` surface over a
//! shell's sidebar. Left for the browser crate: the `nav` element, group and separator
//! elements, button and badge elements, tooltips (side `left`) and CSS, and running an entry
//! on click (an action's `run` gets a fresh context, then the sidebar redraws).
//!
//! An action's `active` gate that panics is not caught (the original treats a throw as
//! `false`).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use super::contributions::{
    Contributions, SidePanelButton, SidebarBadge, SidebarControl, WidgetActionTarget, WidgetContext,
};
use super::panel_dock::PanelChrome;
use super::sidebar_composition::{
    SidebarComposition, SidebarKind, SidebarLayout, SidebarSource, compose_sidebar,
};
use crate::i18n::{MessageLike, Translator, msg, text_of};

/// The sidebar's width.
pub const SIDEBAR_WIDTH_PX: f64 = 44.0;

/// The `nav` element's accessible name.
pub fn sidebar_label() -> MessageLike {
    msg("vela.sidebar.label", "Sidebar")
}

/// One button.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarButtonEntry {
    pub id: String,
    pub kind: SidebarKind,
    pub order: f64,
    pub title: String,
    pub icon: String,
    /// The pressed look (a panel that is open, an action's `active`).
    pub active: bool,
}

impl SidebarSource for SidebarButtonEntry {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> SidebarKind {
        self.kind
    }
    fn order(&self) -> f64 {
        self.order
    }
}

/// A badge's text: `None`, `0` and `""` clear it; numbers above 99 show as `99+`.
pub fn badge_text(badge: Option<&SidebarBadge>) -> Option<String> {
    match badge? {
        SidebarBadge::Text(t) if t.is_empty() => None,
        SidebarBadge::Text(t) => Some(t.clone()),
        SidebarBadge::Number(n) if *n == 0.0 => None,
        SidebarBadge::Number(n) if *n > 99.0 => Some("99+".to_string()),
        SidebarBadge::Number(n) => Some(crate::util::js_number(*n)),
    }
}

/// The sidebar actions registered for the shell, `when`-filtered by `ctx`, in the
/// registry's order (the index keeps it as the flow order).
pub fn sidebar_actions(
    contributions: &Contributions,
    ctx: &dyn WidgetContext,
) -> Vec<Rc<super::contributions::WidgetActionDescriptor>> {
    contributions.widget_actions(WidgetActionTarget::Sidebar, Some(ctx))
}

/// The sidebar's state.
#[derive(Default)]
pub struct SidebarState {
    composition: SidebarComposition,
    panels: RefCell<Vec<SidePanelButton>>,
    open: RefCell<BTreeSet<String>>,
    badges: RefCell<BTreeMap<String, String>>,
    /// Entries a plugin hid at runtime (`ctx.sidebar.set_visible`).
    hidden: RefCell<BTreeSet<String>>,
    /// The ids that made it onto the last layout: what `is_visible` reports.
    shown: RefCell<HashSet<String>>,
    /// The shell finished building: the actions (which read the context) may show.
    live: Cell<bool>,
}

impl SidebarState {
    pub fn new(composition: SidebarComposition) -> Self {
        Self {
            composition,
            ..Self::default()
        }
    }

    /// The dock's panel toggles (dock order).
    pub fn set_panel_buttons(&self, buttons: &[SidePanelButton]) {
        *self.panels.borrow_mut() = buttons.to_vec();
    }

    pub fn set_panel_active(&self, id: &str, open: bool) {
        if open {
            self.open.borrow_mut().insert(id.to_string());
        } else {
            self.open.borrow_mut().remove(id);
        }
    }

    /// Returns whether the badge changed (a redraw is owed).
    pub fn set_badge(&self, id: &str, badge: Option<&SidebarBadge>) -> bool {
        let text = badge_text(badge);
        let mut badges = self.badges.borrow_mut();
        if badges.get(id).cloned() == text {
            return false;
        }
        match text {
            Some(t) => {
                badges.insert(id.to_string(), t);
            }
            None => {
                badges.remove(id);
            }
        }
        true
    }

    pub fn badge(&self, id: &str) -> Option<String> {
        self.badges.borrow().get(id).cloned()
    }

    /// Whether an entry shows right now: on the last layout.
    pub fn is_visible(&self, id: &str) -> bool {
        self.shown.borrow().contains(id)
    }

    /// Hide an entry (`false`) or give it back (`true`). Returns whether it changed (a
    /// redraw is owed).
    pub fn set_visible(&self, id: &str, visible: bool) -> bool {
        let currently_visible = !self.hidden.borrow().contains(id);
        if visible == currently_visible {
            return false;
        }
        if visible {
            self.hidden.borrow_mut().remove(id);
        } else {
            self.hidden.borrow_mut().insert(id.to_string());
        }
        true
    }

    /// The shell is built: show the actions from now on.
    pub fn activate(&self) {
        self.live.set(true);
    }

    /// Lay out the panel toggles and, once live, the sidebar actions. Records which ids are
    /// shown (what [`is_visible`](Self::is_visible) answers) and returns the layout to paint.
    pub fn layout(
        &self,
        contributions: &Contributions,
        ctx: Option<&dyn WidgetContext>,
        i18n: Option<&dyn Translator>,
    ) -> SidebarLayout<SidebarButtonEntry> {
        let open = self.open.borrow();
        let mut entries: Vec<SidebarButtonEntry> = self
            .panels
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, p)| SidebarButtonEntry {
                id: p.id.clone(),
                kind: SidebarKind::Panel,
                order: i as f64,
                title: p.title.clone(),
                icon: p.icon.clone(),
                active: open.contains(&p.id),
            })
            .collect();
        if let (true, Some(ctx)) = (self.live.get(), ctx) {
            for (i, a) in sidebar_actions(contributions, ctx).into_iter().enumerate() {
                entries.push(SidebarButtonEntry {
                    id: a.id.clone(),
                    kind: SidebarKind::Action,
                    order: i as f64,
                    title: text_of(i18n, &a.label, &[]),
                    icon: a.icon.clone().unwrap_or_else(|| "kebab".to_string()),
                    active: a.active.as_ref().is_some_and(|f| f(ctx)),
                });
            }
        }
        let hidden = self.hidden.borrow();
        entries.retain(|e| !hidden.contains(&e.id));
        let layout = compose_sidebar(&entries, &self.composition);
        *self.shown.borrow_mut() = layout
            .top
            .iter()
            .chain(layout.bottom.iter())
            .flatten()
            .map(|e| e.id.clone())
            .collect();
        layout
    }
}

impl PanelChrome for SidebarState {
    fn set_panel_buttons(&self, buttons: &[SidePanelButton]) {
        SidebarState::set_panel_buttons(self, buttons);
    }

    fn set_panel_active(&self, id: &str, open: bool) {
        SidebarState::set_panel_active(self, id, open);
    }
}

/// `ctx.sidebar` over a shell's sidebar, if it has one (read at call time). `repaint` redraws
/// the sidebar from its current state.
pub struct SidebarHandle {
    get: Rc<dyn Fn() -> Option<Rc<SidebarState>>>,
    repaint: Rc<dyn Fn()>,
}

impl SidebarHandle {
    pub fn new(
        get: impl Fn() -> Option<Rc<SidebarState>> + 'static,
        repaint: impl Fn() + 'static,
    ) -> Self {
        Self {
            get: Rc::new(get),
            repaint: Rc::new(repaint),
        }
    }
}

impl SidebarControl for SidebarHandle {
    fn enabled(&self) -> bool {
        (self.get)().is_some()
    }

    fn set_badge(&self, id: &str, badge: Option<SidebarBadge>) {
        if let Some(sidebar) = (self.get)()
            && sidebar.set_badge(id, badge.as_ref())
        {
            (self.repaint)();
        }
    }

    fn refresh(&self) {
        if (self.get)().is_some() {
            (self.repaint)();
        }
    }

    fn is_visible(&self, id: &str) -> bool {
        (self.get)().is_some_and(|s| s.is_visible(id))
    }

    fn set_visible(&self, id: &str, visible: bool) {
        if let Some(sidebar) = (self.get)()
            && sidebar.set_visible(id, visible)
        {
            (self.repaint)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;

    fn contributions() -> Contributions {
        Contributions::new(ManualPlatform::new(0.0).handle())
    }

    fn buttons() -> Vec<SidePanelButton> {
        ["objects", "dataWindow", "notes"]
            .iter()
            .map(|id| SidePanelButton {
                id: (*id).into(),
                title: id.to_uppercase(),
                icon: "i".into(),
            })
            .collect()
    }

    fn ids(groups: &[Vec<SidebarButtonEntry>]) -> Vec<Vec<&str>> {
        groups
            .iter()
            .map(|g| g.iter().map(|e| e.id.as_str()).collect())
            .collect()
    }

    #[test]
    fn badges_normalize_clear_and_cap() {
        assert_eq!(badge_text(None), None);
        assert_eq!(badge_text(Some(&SidebarBadge::Number(0.0))), None);
        assert_eq!(badge_text(Some(&SidebarBadge::Text(String::new()))), None);
        assert_eq!(
            badge_text(Some(&SidebarBadge::Number(7.0))).as_deref(),
            Some("7")
        );
        assert_eq!(
            badge_text(Some(&SidebarBadge::Number(99.0))).as_deref(),
            Some("99")
        );
        assert_eq!(
            badge_text(Some(&SidebarBadge::Number(100.0))).as_deref(),
            Some("99+")
        );
        assert_eq!(
            badge_text(Some(&SidebarBadge::Text("new".into()))).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn setting_a_badge_reports_only_a_change() {
        let s = SidebarState::default();
        assert!(!s.set_badge("objects", None));
        assert!(s.set_badge("objects", Some(&SidebarBadge::Number(3.0))));
        assert!(!s.set_badge("objects", Some(&SidebarBadge::Number(3.0))));
        assert_eq!(s.badge("objects").as_deref(), Some("3"));
        assert!(s.set_badge("objects", Some(&SidebarBadge::Number(0.0))));
        assert_eq!(s.badge("objects"), None);
    }

    #[test]
    fn the_default_composition_puts_inspection_panels_below_and_the_rest_on_top() {
        let s = SidebarState::default();
        s.set_panel_buttons(&buttons());
        let layout = s.layout(&contributions(), None, None);
        assert_eq!(ids(&layout.top), [vec!["notes"]]);
        assert_eq!(ids(&layout.bottom), [vec!["objects", "dataWindow"]]);
        assert!(s.is_visible("objects") && s.is_visible("notes") && !s.is_visible("ghost"));
    }

    #[test]
    fn a_host_composition_places_the_entries() {
        let comp = SidebarComposition {
            top: Some(vec!["objects".into(), "|".into(), "notes".into()]),
            bottom: Some(vec!["dataWindow".into()]),
        };
        let s = SidebarState::new(comp);
        s.set_panel_buttons(&buttons());
        let layout = s.layout(&contributions(), None, None);
        assert_eq!(ids(&layout.top), [vec!["objects"], vec!["notes"]]);
        assert_eq!(ids(&layout.bottom), [vec!["dataWindow"]]);
    }

    #[test]
    fn the_pressed_look_follows_the_open_panels() {
        let s = SidebarState::default();
        s.set_panel_buttons(&buttons());
        s.set_panel_active("objects", true);
        let layout = s.layout(&contributions(), None, None);
        let active: Vec<&str> = layout
            .bottom
            .iter()
            .flatten()
            .filter(|e| e.active)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(active, ["objects"]);
        s.set_panel_active("objects", false);
        let layout = s.layout(&contributions(), None, None);
        assert!(!layout.bottom.iter().flatten().any(|e| e.active));
    }

    #[test]
    fn hiding_an_entry_removes_it_until_it_is_given_back() {
        let s = SidebarState::default();
        s.set_panel_buttons(&buttons());
        assert!(!s.set_visible("objects", true), "already visible");
        assert!(s.set_visible("objects", false));
        assert!(!s.set_visible("objects", false));
        s.layout(&contributions(), None, None);
        assert!(!s.is_visible("objects") && s.is_visible("dataWindow"));
        assert!(s.set_visible("objects", true));
        s.layout(&contributions(), None, None);
        assert!(s.is_visible("objects"));
    }

    #[test]
    fn the_handle_is_inert_without_a_sidebar_and_repaints_on_a_change() {
        let state: Rc<RefCell<Option<Rc<SidebarState>>>> = Rc::new(RefCell::new(None));
        let repaints = Rc::new(Cell::new(0));
        let (get, count) = (Rc::clone(&state), Rc::clone(&repaints));
        let handle = SidebarHandle::new(
            move || get.borrow().clone(),
            move || count.set(count.get() + 1),
        );
        assert!(!handle.enabled());
        handle.set_badge("objects", Some(SidebarBadge::Number(3.0)));
        handle.set_visible("objects", false);
        handle.refresh();
        assert_eq!(repaints.get(), 0);
        assert!(!handle.is_visible("objects"));

        let sidebar = Rc::new(SidebarState::default());
        sidebar.set_panel_buttons(&buttons());
        sidebar.layout(&contributions(), None, None);
        *state.borrow_mut() = Some(Rc::clone(&sidebar));
        assert!(handle.enabled());
        handle.set_badge("objects", Some(SidebarBadge::Number(3.0)));
        handle.set_badge("objects", Some(SidebarBadge::Number(3.0)));
        assert_eq!(repaints.get(), 1, "an unchanged badge repaints nothing");
        handle.set_visible("objects", false);
        handle.set_visible("objects", false);
        assert_eq!(repaints.get(), 2);
        handle.refresh();
        assert_eq!(repaints.get(), 3);
        assert!(handle.is_visible("dataWindow"));
    }

    #[test]
    fn the_sidebar_is_a_panel_chrome_and_names_itself() {
        let s = SidebarState::default();
        let chrome: &dyn PanelChrome = &s;
        chrome.set_panel_buttons(&buttons());
        chrome.set_panel_active("notes", true);
        assert_eq!(SIDEBAR_WIDTH_PX, 44.0);
        assert_eq!(text_of(None, &sidebar_label(), &[]), "Sidebar");
        s.activate();
    }
}
