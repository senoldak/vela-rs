//! The public pane control surface, `chart.panes`: lists panes with their
//! indicators and moves, merges, reorders, collapses and maximizes them. On a
//! renderer without pane management every mutation warns and does nothing (and
//! `list()` still reports the panes).
//!
//! The control holds its controller (the orchestrator) as a [`Weak`], so a
//! `chart.panes` kept by user code after the chart is destroyed does not keep
//! the engine alive. Once the controller is gone the control is inert and
//! silent: `supported` is false, `list` is empty, mutations do nothing.

use std::rc::{Rc, Weak};

use crate::options::PaneInfo;
use crate::ports::chart_renderer::{MoveTarget, PaneMoveDir};
use crate::runtime::Platform;

/// Callback surface [`PanesControl`] uses to drive the orchestrator.
pub trait PaneController {
    /// Whether the active renderer supports pane management (move, merge,
    /// reorder, collapse).
    fn pane_management_supported(&self) -> bool;
    fn list_panes(&self) -> Vec<PaneInfo>;
    fn move_pane(&self, pane_id: &str, dir: PaneMoveDir);
    fn remove_pane_and_indicators(&self, pane_id: &str);
    fn collapse_pane(&self, pane_id: &str, collapsed: bool);
    fn maximize_pane(&self, pane_id: Option<&str>);
    fn set_pane_height_weight(&self, pane_id: &str, weight: f64);
    fn move_indicator(&self, id: &str, target: MoveTarget);
}

/// `chart.panes`. A cheap handle.
#[derive(Clone)]
pub struct PanesControl {
    controller: Weak<dyn PaneController>,
    platform: Rc<dyn Platform>,
}

impl PanesControl {
    /// `platform` receives the unsupported-renderer warnings.
    pub fn new(controller: Weak<dyn PaneController>, platform: Rc<dyn Platform>) -> Self {
        Self {
            controller,
            platform,
        }
    }

    /// Whether the active renderer supports pane management (use to show or hide
    /// host UI).
    pub fn supported(&self) -> bool {
        self.controller
            .upgrade()
            .is_some_and(|c| c.pane_management_supported())
    }

    /// The current panes (top-to-bottom by `order`) with the indicators each holds.
    pub fn list(&self) -> Vec<PaneInfo> {
        self.controller
            .upgrade()
            .map(|c| c.list_panes())
            .unwrap_or_default()
    }

    /// Move a pane one slot up or down in the stack (the price pane stays pinned on top).
    pub fn move_pane(&self, pane_id: &str, dir: PaneMoveDir) -> &Self {
        if let Some(c) = self.guard() {
            c.move_pane(pane_id, dir);
        }
        self
    }

    /// Remove a pane and every indicator in it (the price pane cannot be removed).
    pub fn remove(&self, pane_id: &str) -> &Self {
        if let Some(c) = self.guard() {
            c.remove_pane_and_indicators(pane_id);
        }
        self
    }

    /// Collapse a pane to a thin strip, or restore it (`collapse(id, true)` is
    /// the original's default).
    pub fn collapse(&self, pane_id: &str, collapsed: bool) -> &Self {
        if let Some(c) = self.guard() {
            c.collapse_pane(pane_id, collapsed);
        }
        self
    }

    /// Maximize one pane to fill the plot, or restore the split (`None`).
    pub fn maximize(&self, pane_id: Option<&str>) -> &Self {
        if let Some(c) = self.guard() {
            c.maximize_pane(pane_id);
        }
        self
    }

    /// Set a pane's height weight: the panes share the plot height in proportion
    /// to their weights, so `set_height_weight("price", 6.0)` next to two study
    /// panes at 1 gives the price pane three quarters of the height. The weight
    /// sticks until the user drags a separator or it is set again; a collapsed
    /// pane applies it when expanded. `weight` must be a positive number.
    pub fn set_height_weight(&self, pane_id: &str, weight: f64) -> &Self {
        if let Some(c) = self.guard() {
            c.set_pane_height_weight(pane_id, weight);
        }
        self
    }

    /// Move or merge an indicator to a pane: sugar for `handle.move_to(...)` by id.
    pub fn move_indicator(&self, id: &str, target: MoveTarget) -> &Self {
        if let Some(c) = self.guard() {
            c.move_indicator(id, target);
        }
        self
    }

    /// The controller when pane management is available, else warn (once per
    /// call) and `None`. A destroyed chart is silent.
    fn guard(&self) -> Option<Rc<dyn PaneController>> {
        let controller = self.controller.upgrade()?;
        if controller.pane_management_supported() {
            return Some(controller);
        }
        self.platform.warn(
            "[vela] the active renderer does not support pane management \u{2014} chart.panes mutation ignored.",
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::model::PaneKind;
    use crate::options::PaneIndicatorInfo;
    use crate::ports::chart_renderer::NewPanePlacement;
    use crate::runtime::testing::ManualPlatform;

    #[derive(Default)]
    struct Recorder {
        supported: Cell<bool>,
        calls: RefCell<Vec<String>>,
    }

    impl PaneController for Recorder {
        fn pane_management_supported(&self) -> bool {
            self.supported.get()
        }
        fn list_panes(&self) -> Vec<PaneInfo> {
            vec![PaneInfo {
                id: "price".into(),
                kind: PaneKind::Price,
                order: 0,
                collapsed: false,
                maximized: false,
                height_weight: Some(3.0),
                indicators: vec![PaneIndicatorInfo {
                    id: "volume".into(),
                    title: "Volume".into(),
                    shorttitle: None,
                    own_scale: true,
                }],
            }]
        }
        fn move_pane(&self, pane_id: &str, dir: PaneMoveDir) {
            self.calls
                .borrow_mut()
                .push(format!("move {pane_id} {dir:?}"));
        }
        fn remove_pane_and_indicators(&self, pane_id: &str) {
            self.calls.borrow_mut().push(format!("remove {pane_id}"));
        }
        fn collapse_pane(&self, pane_id: &str, collapsed: bool) {
            self.calls
                .borrow_mut()
                .push(format!("collapse {pane_id} {collapsed}"));
        }
        fn maximize_pane(&self, pane_id: Option<&str>) {
            self.calls
                .borrow_mut()
                .push(format!("maximize {pane_id:?}"));
        }
        fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
            self.calls
                .borrow_mut()
                .push(format!("weight {pane_id} {weight}"));
        }
        fn move_indicator(&self, id: &str, target: MoveTarget) {
            self.calls
                .borrow_mut()
                .push(format!("moveIndicator {id} {target:?}"));
        }
    }

    fn rig(supported: bool) -> (ManualPlatform, Rc<Recorder>, PanesControl) {
        let platform = ManualPlatform::new(0.0);
        let controller = Rc::new(Recorder::default());
        controller.supported.set(supported);
        let as_dyn: Rc<dyn PaneController> = controller.clone();
        let panes = PanesControl::new(Rc::downgrade(&as_dyn), platform.handle());
        (platform, controller, panes)
    }

    #[test]
    fn mutations_drive_the_controller_and_chain() {
        let (platform, controller, panes) = rig(true);
        assert!(panes.supported());
        panes
            .move_pane("p2", PaneMoveDir::Up)
            .remove("p3")
            .collapse("p2", true)
            .maximize(Some("p2"))
            .maximize(None)
            .set_height_weight("price", 6.0)
            .move_indicator("ind-1", MoveTarget::Pane("p2".into()))
            .move_indicator("ind-2", MoveTarget::NewPane(NewPanePlacement::default()));
        assert_eq!(
            *controller.calls.borrow(),
            [
                "move p2 Up",
                "remove p3",
                "collapse p2 true",
                "maximize Some(\"p2\")",
                "maximize None",
                "weight price 6",
                "moveIndicator ind-1 Pane(\"p2\")",
                "moveIndicator ind-2 NewPane(NewPanePlacement { before: None, after: None })",
            ]
        );
        assert!(platform.warnings().is_empty());
    }

    #[test]
    fn list_reports_the_panes_whatever_the_renderer_supports() {
        let (_platform, _controller, panes) = rig(false);
        let listed = panes.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].indicators[0].id, "volume");
        assert!(!panes.supported());
    }

    #[test]
    fn without_pane_management_every_mutation_warns_and_does_nothing() {
        let (platform, controller, panes) = rig(false);
        panes
            .move_pane("p", PaneMoveDir::Down)
            .remove("p")
            .collapse("p", false)
            .maximize(None)
            .set_height_weight("p", 2.0)
            .move_indicator("i", MoveTarget::Price);
        assert!(controller.calls.borrow().is_empty());
        let warnings = platform.warnings();
        assert_eq!(warnings.len(), 6);
        assert!(warnings.iter().all(|w| {
            w == "[vela] the active renderer does not support pane management \u{2014} chart.panes mutation ignored."
        }));
    }

    #[test]
    fn support_is_read_per_call() {
        let (platform, controller, panes) = rig(false);
        panes.remove("p");
        controller.supported.set(true);
        panes.remove("p");
        assert_eq!(*controller.calls.borrow(), ["remove p"]);
        assert_eq!(platform.warnings().len(), 1);
    }

    #[test]
    fn a_control_outliving_its_chart_is_inert_silent_and_does_not_keep_it_alive() {
        let (platform, controller, panes) = rig(true);
        let weak = Rc::downgrade(&controller);
        drop(controller);
        assert!(
            weak.upgrade().is_none(),
            "the control must not own the controller"
        );
        assert!(!panes.supported());
        assert!(panes.list().is_empty());
        panes.remove("p").move_indicator("i", MoveTarget::Price);
        assert!(platform.warnings().is_empty());
    }
}
