//! Picking the drawing under a pixel, and what a delete removes.
//!
//! The original's selection sets (`Set<string>`, insertion ordered) are slices of unique
//! ids in selection order here, so the order of the ids a delete reports is the same.

use crate::drawings::{Drawing, Projector};

/// Pixel tolerance for grabbing a drawing body / handle.
pub const HIT_TOLERANCE: f64 = 6.0;

/// The topmost drawing whose body is within `tol` px of `(x, y)`, searching front to
/// back (highest z first). Hidden drawings are skipped. `drawings` is expected in
/// ascending paint order (the store's `all()`), so this iterates from the end.
pub fn top_drawing_at<'a>(
    drawings: &[&'a dyn Drawing],
    x: f64,
    y: f64,
    proj: &dyn Projector,
    tol: f64,
) -> Option<&'a dyn Drawing> {
    for d in drawings.iter().rev() {
        if !d.base().visible {
            continue;
        }
        // A drawing on a hidden pane (collapsed / zeroed by a maximize) isn't painted:
        // don't let an invisible body swallow presses meant for the visible pane
        // underneath.
        if let Some(rect) = proj.pane_rect(&d.base().pane_id)
            && rect.height <= 0.0
        {
            continue;
        }
        if d.hit_test(x, y, proj, tol) {
            return Some(*d);
        }
    }
    None
}

fn is_locked(id: &str, drawings: &[&dyn Drawing]) -> bool {
    drawings
        .iter()
        .find(|d| d.base().id == id)
        .is_some_and(|d| d.base().locked)
}

/// What a selection-wide delete removes: a lone selected drawing goes regardless, but
/// inside a multi-selection a LOCKED drawing is protected (the others go and it stays,
/// selected).
pub fn deletable_selection(selected: &[String], drawings: &[&dyn Drawing]) -> Vec<String> {
    if selected.len() < 2 {
        return selected.to_vec();
    }
    selected
        .iter()
        .filter(|id| !is_locked(id, drawings))
        .cloned()
        .collect()
}

/// What a delete-at-cursor press removes when `hit` is the drawing under it. A hit on a
/// member of a multi-selection (with `with_selection`) takes the selection's deletable
/// members, even when the hit itself is locked, so a middle-click on the locked one
/// still clears the rest of the group. Any other locked hit is protected and nothing is
/// removed.
pub fn delete_targets(
    hit: &dyn Drawing,
    selected: &[String],
    drawings: &[&dyn Drawing],
    with_selection: bool,
) -> Vec<String> {
    if with_selection && selected.len() >= 2 && selected.contains(&hit.base().id) {
        return deletable_selection(selected, drawings);
    }
    if hit.base().locked {
        Vec::new()
    } else {
        vec![hit.base().id.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::{DrawingInit, DrawingPoint, DrawingRegistry, LinearProjector};

    fn line(id: &str, locked: bool) -> Box<dyn Drawing> {
        let mut d = DrawingRegistry::builtin()
            .create(
                "trendline",
                DrawingInit {
                    id: Some(id.to_string()),
                    anchors: vec![
                        DrawingPoint {
                            time: 0.0,
                            price: 100.0,
                        },
                        DrawingPoint {
                            time: 100.0 * 60_000.0,
                            price: 100.0,
                        },
                    ],
                    ..DrawingInit::on_pane("price")
                },
            )
            .expect("trendline is registered");
        d.base_mut().locked = locked;
        d
    }

    fn refs(ds: &[Box<dyn Drawing>]) -> Vec<&dyn Drawing> {
        ds.iter().map(|d| d.as_ref()).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_topmost_visible_drawing_under_the_pixel_wins() {
        let proj = LinearProjector::sample();
        // y(100) = (200 - 100) / 0.25 = 400.
        let ds = vec![line("a", false), line("b", false)];
        let all = refs(&ds);
        let hit = top_drawing_at(&all, 50.0, 400.0, &proj, HIT_TOLERANCE).unwrap();
        assert_eq!(hit.base().id, "b");
        assert!(top_drawing_at(&all, 50.0, 300.0, &proj, HIT_TOLERANCE).is_none());
    }

    #[test]
    fn hidden_drawings_are_skipped() {
        let proj = LinearProjector::sample();
        let mut ds = vec![line("a", false), line("b", false)];
        ds[1].base_mut().visible = false;
        let all = refs(&ds);
        let hit = top_drawing_at(&all, 50.0, 400.0, &proj, HIT_TOLERANCE).unwrap();
        assert_eq!(hit.base().id, "a");
    }

    #[test]
    fn a_drawing_on_a_zero_height_pane_cannot_be_hit() {
        struct Hidden(LinearProjector);
        impl Projector for Hidden {
            fn x_of(&self, t: f64) -> f64 {
                self.0.x_of(t)
            }
            fn y_of(&self, p: f64, id: &str) -> Option<f64> {
                self.0.y_of(p, id)
            }
            fn px_to_point(&self, x: f64, y: f64, id: &str) -> DrawingPoint {
                self.0.px_to_point(x, y, id)
            }
            fn pane_id_at_y(&self, y: f64) -> Option<String> {
                self.0.pane_id_at_y(y)
            }
            fn pane_rect(&self, _: &str) -> Option<crate::drawings::PaneRect> {
                Some(crate::drawings::PaneRect {
                    top: 0.0,
                    height: 0.0,
                })
            }
            fn width(&self) -> f64 {
                self.0.width()
            }
            fn height(&self) -> f64 {
                self.0.height()
            }
        }
        let ds = vec![line("a", false)];
        let all = refs(&ds);
        let proj = Hidden(LinearProjector::sample());
        assert!(top_drawing_at(&all, 50.0, 400.0, &proj, HIT_TOLERANCE).is_none());
    }

    #[test]
    fn a_lone_unlocked_hit_goes_and_a_lone_locked_hit_is_protected() {
        let ds = vec![line("a", false), line("b", false)];
        let all = refs(&ds);
        assert_eq!(delete_targets(all[0], &[], &all, false), ids(&["a"]));
        let locked = line("c", true);
        assert!(delete_targets(locked.as_ref(), &[], &all, false).is_empty());
    }

    #[test]
    fn a_middle_click_on_a_locked_member_of_a_multi_selection_removes_the_unlocked_members() {
        let ds = vec![line("a", true), line("b", false), line("c", false)];
        let all = refs(&ds);
        let sel = ids(&["a", "b", "c"]);
        assert_eq!(delete_targets(all[0], &sel, &all, true), ids(&["b", "c"]));
        assert_eq!(delete_targets(all[1], &sel, &all, true), ids(&["b", "c"]));
    }

    #[test]
    fn an_all_locked_multi_selection_removes_nothing() {
        let ds = vec![line("a", true), line("b", true)];
        let all = refs(&ds);
        let sel = ids(&["a", "b"]);
        assert!(delete_targets(all[0], &sel, &all, true).is_empty());
        assert!(deletable_selection(&sel, &all).is_empty());
    }

    #[test]
    fn the_eraser_never_widens_to_the_selection() {
        let ds = vec![line("a", false), line("b", false)];
        let all = refs(&ds);
        let sel = ids(&["a", "b"]);
        assert_eq!(delete_targets(all[0], &sel, &all, false), ids(&["a"]));
        let outsider = line("c", false);
        assert_eq!(
            delete_targets(outsider.as_ref(), &sel, &all, true),
            ids(&["c"])
        );
    }

    #[test]
    fn a_lone_selected_drawing_is_deleted_even_when_locked() {
        let ds = vec![line("a", true)];
        let all = refs(&ds);
        assert_eq!(deletable_selection(&ids(&["a"]), &all), ids(&["a"]));
        assert!(deletable_selection(&[], &all).is_empty());
    }
}
