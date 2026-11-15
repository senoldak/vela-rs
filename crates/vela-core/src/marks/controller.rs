//! The renderer-facing owner of the timeline-mark model (`MarksController` of
//! the original, backing `chart.marks`).
//!
//! The renderer-free state (marks, groups, the validation) is
//! [`MarksModel`]. This controller pushes snapshots of it to the renderer
//! through its optional `set_timeline_marks` seam, re-emits the renderer's
//! glyph clicks as `mark:click`, and routes group visibility through the
//! renderer's `marks` feature. On a renderer without the `timeline_marks`
//! capability it is inert: the model still fills, so `all()` and a later host UI
//! see it.
//!
//! The controller is a cheap-to-clone shared handle (`&self` methods). Where the
//! original throws on a malformed mark or group, the methods return
//! [`MarksError`] and leave the renderer untouched, as the throw does.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value;

use crate::events::types::{ChartEvent, ChartEventBus};
use crate::ports::chart_renderer::ChartRenderer;
use crate::runtime::{Platform, Unsubscribe};

use super::{MarkGroup, MarkGroupChoices, MarksError, MarksModel, TimelineMark};

struct Inner {
    renderer: Rc<dyn ChartRenderer>,
    platform: Rc<dyn Platform>,
    enabled: bool,
    model: RefCell<MarksModel>,
    subscription: RefCell<Option<Unsubscribe>>,
}

/// Renderer-agnostic owner of the timeline-mark model. A shared handle.
#[derive(Clone)]
pub struct MarksController {
    inner: Rc<Inner>,
}

impl MarksController {
    /// Build the controller. On a capable renderer the glyph clicks are
    /// re-emitted on `events` as `mark:click` until [`destroy`](Self::destroy).
    /// `platform` carries the developer warning of
    /// [`set_group_visible`](Self::set_group_visible).
    pub fn new(
        renderer: Rc<dyn ChartRenderer>,
        events: ChartEventBus,
        platform: Rc<dyn Platform>,
    ) -> Self {
        let enabled =
            renderer.capabilities().timeline_marks && renderer.supports_set_timeline_marks();
        let subscription = if enabled {
            renderer.on_mark_click(Box::new(move |event| {
                let event = ChartEvent::MarkClick(event.clone());
                events.emit(&event.name(), &event);
            }))
        } else {
            None
        };
        Self {
            inner: Rc::new(Inner {
                renderer,
                platform,
                enabled,
                model: RefCell::new(MarksModel::new()),
                subscription: RefCell::new(subscription),
            }),
        }
    }

    /// Whether the active renderer paints timeline marks.
    pub fn supported(&self) -> bool {
        self.inner.enabled
    }

    /// Add (or replace, by id) one mark.
    pub fn add(&self, mark: TimelineMark) -> Result<(), MarksError> {
        self.inner.model.borrow_mut().add(mark)?;
        self.sync();
        Ok(())
    }

    /// Replace the whole set (a market switch). A malformed mark stops the call
    /// after the marks before it were added, and nothing is pushed (the original
    /// throws mid-loop).
    pub fn set(&self, marks: Vec<TimelineMark>) -> Result<(), MarksError> {
        self.inner.model.borrow_mut().set(marks)?;
        self.sync();
        Ok(())
    }

    /// Remove one mark; whether it was there.
    pub fn remove(&self, id: &str) -> bool {
        let had = self.inner.model.borrow_mut().remove(id);
        if had {
            self.sync();
        }
        had
    }

    /// Drop every mark (no push when there were none).
    pub fn clear(&self) {
        let had = self.inner.model.borrow_mut().clear();
        if had {
            self.sync();
        }
    }

    /// Every mark, in insertion order (copies: mutating one changes nothing).
    pub fn all(&self) -> Vec<TimelineMark> {
        self.inner.model.borrow().all().to_vec()
    }

    /// Define (or replace) a group's presentation: its settings label and
    /// default visibility.
    pub fn define_group(&self, group: MarkGroup) -> Result<(), MarksError> {
        self.inner.model.borrow_mut().define_group(group)?;
        self.sync();
        Ok(())
    }

    /// The defined groups, in definition order.
    pub fn group_definitions(&self) -> Vec<MarkGroup> {
        self.inner.model.borrow().group_definitions().to_vec()
    }

    /// Show or hide one group's marks. The choice lives in the renderer's
    /// cosmetic config (the `marks` feature): what the settings dialog's Events
    /// checkboxes edit and what a persisted chart restores. It warns and no-ops
    /// on a renderer without it.
    pub fn set_group_visible(&self, id: &str, visible: bool) {
        let renderer = &self.inner.renderer;
        if !renderer.features().iter().any(|f| f == "marks") {
            self.inner.platform.warn(&format!(
                "[vela] renderer \"{}\" does not paint timeline marks \u{2014} setGroupVisible ignored.",
                renderer.name()
            ));
            return;
        }
        let mut groups = serde_json::Map::new();
        groups.insert(id.to_string(), Value::Bool(visible));
        let mut patch = serde_json::Map::new();
        patch.insert("groups".to_string(), Value::Object(groups));
        renderer.apply_feature("marks", &Value::Object(patch));
    }

    /// A group's effective visibility: its own switch (the user's persisted
    /// choice, else the declared default, else visible) AND every ancestor's, so
    /// a child under a switched-off parent reads hidden whatever its own choice
    /// says.
    pub fn is_group_visible(&self, id: &str) -> bool {
        let mut choices = MarkGroupChoices::new();
        let state = self.inner.renderer.read_feature("marks");
        if let Some(Value::Object(groups)) = state.as_ref().and_then(|s| s.get("groups")) {
            for (gid, v) in groups {
                if let Value::Bool(b) = v {
                    choices.insert(gid.clone(), *b);
                }
            }
        }
        self.inner.model.borrow().is_group_visible(&choices, id)
    }

    /// Stop re-emitting the renderer's glyph clicks.
    pub fn destroy(&self) {
        let subscription = self.inner.subscription.borrow_mut().take();
        if let Some(unsubscribe) = subscription {
            unsubscribe();
        }
    }

    fn sync(&self) {
        if !self.inner.enabled {
            return;
        }
        let (marks, groups) = {
            let model = self.inner.model.borrow();
            (model.all().to_vec(), model.group_definitions().to_vec())
        };
        self.inner.renderer.set_timeline_marks(&marks, &groups);
    }
}

#[cfg(test)]
#[allow(clippy::cloned_ref_to_slice_refs, clippy::type_complexity)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::controller::test_support::{FakeOptions, FakeRenderer, MarksPush};
    use crate::events::types::ChartEventName;
    use crate::marks::{MarkClickEvent, MarkGlyph};
    use crate::runtime::testing::ManualPlatform;

    const T: i64 = 1_718_064_000_000; // 2024-06-11

    struct Setup {
        renderer: Rc<FakeRenderer>,
        platform: ManualPlatform,
        events: ChartEventBus,
        ctrl: MarksController,
    }

    fn setup_with(capable: bool) -> Setup {
        let renderer = FakeRenderer::new(FakeOptions {
            user_drawings: false,
            timeline_marks: capable,
        });
        let platform = ManualPlatform::new(0.0);
        let events = ChartEventBus::new();
        let ctrl = MarksController::new(renderer.clone(), events.clone(), platform.handle());
        Setup {
            renderer,
            platform,
            events,
            ctrl,
        }
    }

    fn setup() -> Setup {
        setup_with(true)
    }

    fn glyph() -> MarkGlyph {
        MarkGlyph {
            color: "#2962ff".into(),
            letter: Some("D".into()),
            ..Default::default()
        }
    }

    fn mark(id: &str) -> TimelineMark {
        TimelineMark::new(id, T, glyph())
    }

    fn group(id: &str, label: &str, visible: Option<bool>, parent: Option<&str>) -> MarkGroup {
        MarkGroup {
            id: id.into(),
            label: label.into(),
            visible,
            parent: parent.map(str::to_owned),
        }
    }

    fn mark_ids(push: &MarksPush) -> Vec<&str> {
        push.marks.iter().map(|m| m.id.as_str()).collect()
    }

    fn ids(ctrl: &MarksController) -> Vec<String> {
        ctrl.all().into_iter().map(|m| m.id).collect()
    }

    #[test]
    fn pushes_marks_and_group_definitions_on_every_change_in_insertion_order() {
        let s = setup();
        assert!(s.ctrl.supported());
        s.ctrl.add(mark("b")).expect("b");
        s.ctrl.add(mark("a")).expect("a");
        s.ctrl
            .define_group(group("dividends", "Dividends", None, None))
            .expect("group");
        let pushes = s.renderer.pushes.borrow();
        assert_eq!(pushes.len(), 3);
        assert_eq!(mark_ids(&pushes[2]), ["b", "a"]);
        assert_eq!(
            pushes[2].groups,
            [group("dividends", "Dividends", None, None)]
        );
        drop(pushes);

        // Re-adding an id replaces it in place.
        let mut again = mark("b");
        again.title = Some("B2".into());
        s.ctrl.add(again).expect("b2");
        {
            let pushes = s.renderer.pushes.borrow();
            let labels: Vec<String> = pushes[3]
                .marks
                .iter()
                .map(|m| format!("{}:{}", m.id, m.title.clone().unwrap_or_default()))
                .collect();
            assert_eq!(labels, ["b:B2", "a:"]);
        }
        assert!(s.ctrl.remove("b"));
        assert_eq!(mark_ids(&s.renderer.pushes.borrow()[4]), ["a"]);
        assert!(!s.ctrl.remove("b")); // already gone: no push
        assert_eq!(s.renderer.pushes.borrow().len(), 5);
        s.ctrl.set(vec![mark("x"), mark("y")]).expect("set");
        assert_eq!(mark_ids(&s.renderer.pushes.borrow()[5]), ["x", "y"]);
        s.ctrl.clear();
        assert!(s.renderer.pushes.borrow()[6].marks.is_empty());
        s.ctrl.clear(); // already empty: no push
        assert_eq!(s.renderer.pushes.borrow().len(), 7);
    }

    #[test]
    fn hands_out_copies() {
        let s = setup();
        s.ctrl.add(mark("a")).expect("a");
        let mut copy = s.ctrl.all();
        copy[0].glyph.color = "red".into();
        assert_eq!(s.ctrl.all()[0].glyph.color, "#2962ff");
    }

    #[test]
    fn rejects_marks_and_groups_it_could_not_place_or_draw() {
        let s = setup();
        assert_eq!(s.ctrl.add(mark("")), Err(MarksError::EmptyMarkId));
        assert_eq!(
            s.ctrl.define_group(group("", "x", None, None)),
            Err(MarksError::EmptyGroupId)
        );
        assert_eq!(
            s.ctrl.define_group(group("bad", "Bad", None, Some(""))),
            Err(MarksError::EmptyGroupParent {
                group: "bad".into()
            })
        );
        assert!(
            s.renderer.pushes.borrow().is_empty(),
            "a rejected call pushes nothing"
        );
        assert!(s.ctrl.all().is_empty());
    }

    #[test]
    fn a_malformed_mark_in_set_stops_after_the_marks_before_it_and_pushes_nothing() {
        let s = setup();
        s.ctrl.add(mark("old")).expect("old");
        let pushes = s.renderer.pushes.borrow().len();
        let err = s.ctrl.set(vec![mark("a"), mark(""), mark("c")]);
        assert_eq!(err, Err(MarksError::EmptyMarkId));
        assert_eq!(ids(&s.ctrl), ["a"]);
        assert_eq!(s.renderer.pushes.borrow().len(), pushes);
    }

    #[test]
    fn re_emits_the_renderers_glyph_clicks_and_unsubscribes_on_destroy() {
        let s = setup();
        let seen: Rc<RefCell<Vec<MarkClickEvent>>> = Rc::default();
        {
            let seen = Rc::clone(&seen);
            s.events.on(ChartEventName::MarkClick, move |event| {
                if let ChartEvent::MarkClick(e) = event {
                    seen.borrow_mut().push(e.clone());
                }
            });
        }
        assert!(s.renderer.mark_click_subscribed());
        let click = MarkClickEvent {
            id: "a".into(),
            ids: vec!["a".into(), "b".into()],
            time: T,
            group: Some("dividends".into()),
        };
        s.renderer.click(click.clone());
        assert_eq!(*seen.borrow(), [click]);
        s.ctrl.destroy();
        assert!(!s.renderer.mark_click_subscribed());
        s.ctrl.destroy(); // twice is fine
    }

    #[test]
    fn routes_group_visibility_through_the_renderers_marks_feature() {
        let s = setup();
        s.ctrl
            .define_group(group("quiet", "Quiet", Some(false), None))
            .expect("group");
        assert!(!s.ctrl.is_group_visible("quiet")); // declared default
        assert!(s.ctrl.is_group_visible("other")); // unknown: visible
        s.ctrl.set_group_visible("quiet", true);
        assert_eq!(
            *s.renderer.features_applied.borrow(),
            [("marks".to_string(), json!({ "groups": { "quiet": true } }))]
        );
        assert!(s.ctrl.is_group_visible("quiet")); // the stored choice wins over the default
    }

    #[test]
    fn a_nested_group_reads_hidden_while_its_parent_is_switched_off() {
        let s = setup();
        s.ctrl
            .define_group(group("news", "News", Some(false), None))
            .expect("news");
        s.ctrl
            .define_group(group("news-latest", "Latest news", None, Some("news")))
            .expect("child");
        assert!(!s.ctrl.is_group_visible("news-latest")); // own default on, parent off
        s.ctrl.set_group_visible("news", true);
        assert!(s.ctrl.is_group_visible("news-latest"));
    }

    #[test]
    fn stays_inert_on_a_renderer_without_the_capability() {
        let s = setup_with(false);
        assert!(!s.ctrl.supported());
        s.ctrl.add(mark("a")).expect("a");
        assert_eq!(ids(&s.ctrl), ["a"]);
        assert!(s.renderer.pushes.borrow().is_empty());
        assert!(!s.renderer.mark_click_subscribed());
        s.ctrl.set_group_visible("x", false);
        assert_eq!(
            s.platform.warnings(),
            [
                "[vela] renderer \"fake\" does not paint timeline marks \u{2014} setGroupVisible ignored."
            ]
        );
        assert!(s.renderer.features_applied.borrow().is_empty());
        assert!(s.ctrl.is_group_visible("x"));
    }

    #[test]
    fn a_click_handler_may_call_back_into_the_controller() {
        let s = setup();
        s.ctrl.add(mark("a")).expect("a");
        {
            let ctrl = s.ctrl.clone();
            s.events.on(ChartEventName::MarkClick, move |event| {
                if let ChartEvent::MarkClick(e) = event {
                    ctrl.remove(&e.id);
                }
            });
        }
        s.renderer.click(MarkClickEvent {
            id: "a".into(),
            ids: vec!["a".into()],
            time: T,
            group: None,
        });
        assert!(s.ctrl.all().is_empty());
    }
}
