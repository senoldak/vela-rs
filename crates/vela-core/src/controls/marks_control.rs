//! The chart's timeline-marks control surface (`chart.marks`), a sibling of
//! `chart.drawings` / `chart.panes`. Marks are DATA, not user state: the chart
//! never persists them; re-supply them when the market changes
//! (`market:changed`). The model is core-owned, so every method works on any
//! renderer; on one without the `timeline_marks` capability nothing paints
//! ([`supported`](MarksControl::supported) reports it) and the group visibility
//! setter warns and no-ops.
//!
//! Chainable methods return `&Self` (the original's `this`); those that can
//! reject a malformed mark or group return `Result<&Self, MarksError>`.

use crate::marks::controller::MarksController;
use crate::marks::{MarkGroup, MarksError, TimelineMark};

/// The `chart.marks` facade over a [`MarksController`].
pub struct MarksControl {
    ctrl: MarksController,
}

impl MarksControl {
    pub fn new(ctrl: MarksController) -> Self {
        Self { ctrl }
    }

    /// Whether the active renderer paints timeline marks.
    pub fn supported(&self) -> bool {
        self.ctrl.supported()
    }

    /// Add one mark; an existing id is replaced in place.
    pub fn add(&self, mark: TimelineMark) -> Result<&Self, MarksError> {
        self.ctrl.add(mark)?;
        Ok(self)
    }

    /// Replace the whole set (a market switch).
    pub fn set(&self, marks: Vec<TimelineMark>) -> Result<&Self, MarksError> {
        self.ctrl.set(marks)?;
        Ok(self)
    }

    pub fn remove(&self, id: &str) -> &Self {
        self.ctrl.remove(id);
        self
    }

    pub fn clear(&self) -> &Self {
        self.ctrl.clear();
        self
    }

    /// Every mark, in insertion order.
    pub fn all(&self) -> Vec<TimelineMark> {
        self.ctrl.all()
    }

    /// Define a visibility group's presentation: the label of its checkbox in
    /// chart settings (the Events tab) and its default visibility. Marks may
    /// name a group that was never defined: it then shows its capitalized id.
    pub fn define_group(&self, group: MarkGroup) -> Result<&Self, MarksError> {
        self.ctrl.define_group(group)?;
        Ok(self)
    }

    /// The defined groups, in definition order.
    pub fn groups(&self) -> Vec<MarkGroup> {
        self.ctrl.group_definitions()
    }

    /// Show or hide one group's marks: the same switch as the settings
    /// checkbox, persisted with the chart's config.
    pub fn set_group_visible(&self, id: &str, visible: bool) -> &Self {
        self.ctrl.set_group_visible(id, visible);
        self
    }

    /// A group's effective visibility (the user's choice, else the group's
    /// declared default).
    pub fn is_group_visible(&self, id: &str) -> bool {
        self.ctrl.is_group_visible(id)
    }
}

#[cfg(test)]
#[allow(clippy::cloned_ref_to_slice_refs, clippy::type_complexity)]
mod tests {
    use super::*;
    use crate::drawings::controller::test_support::{FakeOptions, FakeRenderer};
    use crate::events::types::ChartEventBus;
    use crate::marks::MarkGlyph;
    use crate::runtime::testing::ManualPlatform;

    fn mark(id: &str, group: Option<&str>) -> TimelineMark {
        let mut m = TimelineMark::new(
            id,
            1_718_064_000_000,
            MarkGlyph {
                color: "#2962ff".into(),
                letter: Some("D".into()),
                ..Default::default()
            },
        );
        m.group = group.map(str::to_owned);
        m
    }

    #[test]
    fn chains_and_mirrors_the_controller() {
        let renderer = FakeRenderer::new(FakeOptions::default());
        let platform = ManualPlatform::new(0.0);
        let control = MarksControl::new(MarksController::new(
            renderer.clone(),
            ChartEventBus::new(),
            platform.handle(),
        ));
        let out = control
            .define_group(MarkGroup {
                id: "g".into(),
                label: "G".into(),
                visible: None,
                parent: None,
            })
            .and_then(|c| c.add(mark("a", Some("g"))))
            .and_then(|c| c.add(mark("b", None)))
            .expect("chain");
        assert!(std::ptr::eq(out, &control));
        assert!(control.supported());
        let ids: Vec<String> = control.all().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(control.groups().len(), 1);
        assert_eq!(control.groups()[0].label, "G");
        control.remove("a").clear();
        assert!(
            renderer
                .pushes
                .borrow()
                .last()
                .expect("push")
                .marks
                .is_empty()
        );
        control.set_group_visible("g", false);
        assert!(!control.is_group_visible("g"));
        let again = control.set_group_visible("g", true);
        assert!(std::ptr::eq(again, &control));
        assert!(control.is_group_visible("g"));
    }

    #[test]
    fn rejects_what_the_controller_rejects() {
        let renderer = FakeRenderer::new(FakeOptions::default());
        let platform = ManualPlatform::new(0.0);
        let control = MarksControl::new(MarksController::new(
            renderer,
            ChartEventBus::new(),
            platform.handle(),
        ));
        assert_eq!(
            control.add(mark("", None)).err(),
            Some(MarksError::EmptyMarkId)
        );
        assert_eq!(
            control.set(vec![mark("", None)]).err(),
            Some(MarksError::EmptyMarkId)
        );
        assert!(control.all().is_empty());
    }
}
