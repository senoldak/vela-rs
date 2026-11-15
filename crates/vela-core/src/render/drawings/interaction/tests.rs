//! Ports of `drawings-interaction.test.ts` and `drawings-box-handles.test.ts`.

use serde_json::json;

use super::*;
use crate::drawings::types::shapes::MAX_PATH_POINTS;
use crate::drawings::{DEFAULT_DRAWING_COLOR, PaneRect, SerializedDrawing};

/// Linear projector: x = time, y = 100 - price, single pane `price`.
struct FakeProj;

impl Projector for FakeProj {
    fn x_of(&self, time: f64) -> f64 {
        time
    }
    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
        (pane_id == "price").then_some(100.0 - price)
    }
    fn px_to_point(&self, x: f64, y: f64, _pane_id: &str) -> DrawingPoint {
        DrawingPoint {
            time: x,
            price: 100.0 - y,
        }
    }
    fn pane_id_at_y(&self, _y: f64) -> Option<String> {
        Some("price".to_string())
    }
    fn pane_rect(&self, _pane_id: &str) -> Option<PaneRect> {
        None
    }
    fn width(&self) -> f64 {
        200.0
    }
    fn height(&self) -> f64 {
        100.0
    }
}

struct Deps {
    proj: FakeProj,
    tool: Option<String>,
    drawings: Vec<Box<dyn Drawing>>,
    hovered: Option<String>,
    defaults: Option<DrawingToolDefaults>,
    selected: Vec<String>,
    intents: Vec<DrawingIntent>,
    settings: Vec<(String, f64, f64)>,
    changes: usize,
    registry: DrawingRegistry,
}

impl InteractionDeps for Deps {
    fn projector(&self) -> &dyn Projector {
        &self.proj
    }
    fn active_tool(&self) -> Option<String> {
        self.tool.clone()
    }
    fn drawings(&self) -> Vec<&dyn Drawing> {
        self.drawings.iter().map(|d| d.as_ref()).collect()
    }
    fn drawing_mut(&mut self, id: &str) -> Option<&mut dyn Drawing> {
        self.drawings
            .iter_mut()
            .find(|d| d.base().id == id)
            .map(|d| d.as_mut())
    }
    fn hovered_id(&self) -> Option<String> {
        self.hovered.clone()
    }
    fn selected_ids(&self) -> Vec<String> {
        self.selected.clone()
    }
    fn emit(&mut self, intent: DrawingIntent) {
        self.intents.push(intent);
    }
    fn changed(&mut self) {
        self.changes += 1;
    }
    fn open_settings(&mut self, id: &str, x: f64, y: f64) {
        self.settings.push((id.to_string(), x, y));
    }
    // A fake "candle snap": round both axes to the nearest 10. In weak mode only snap when
    // the snapped pixel is within 2px of the cursor.
    fn snap(
        &self,
        point: DrawingPoint,
        _pane_id: &str,
        mode: SnapMode,
        cursor_px: Option<(f64, f64)>,
    ) -> DrawingPoint {
        let snapped = DrawingPoint {
            time: js_round(point.time / 10.0) * 10.0,
            price: js_round(point.price / 10.0) * 10.0,
        };
        if mode == SnapMode::Weak
            && let Some((cx, cy)) = cursor_px
        {
            let dist = (snapped.time - cx).hypot(100.0 - snapped.price - cy);
            if dist > 2.0 {
                return point;
            }
        }
        snapped
    }
    fn tool_defaults(&self) -> Option<DrawingToolDefaults> {
        self.defaults.clone()
    }
    fn registry(&self) -> &DrawingRegistry {
        &self.registry
    }
}

struct Rig {
    it: DrawingInteraction,
    d: Deps,
}

fn pt(time: f64, price: f64) -> DrawingPoint {
    DrawingPoint { time, price }
}

fn make(type_key: &str, id: &str, anchors: &[(f64, f64)]) -> Box<dyn Drawing> {
    DrawingRegistry::builtin()
        .create(
            type_key,
            DrawingInit {
                id: Some(id.to_string()),
                pane_id: "price".to_string(),
                anchors: anchors.iter().map(|a| pt(a.0, a.1)).collect(),
                ..DrawingInit::default()
            },
        )
        .expect("a registered type")
}

fn rig(tool: Option<&str>, drawings: Vec<Box<dyn Drawing>>) -> Rig {
    Rig {
        it: DrawingInteraction::new(),
        d: Deps {
            proj: FakeProj,
            tool: tool.map(str::to_string),
            drawings,
            hovered: None,
            defaults: None,
            selected: Vec::new(),
            intents: Vec::new(),
            settings: Vec::new(),
            changes: 0,
            registry: DrawingRegistry::builtin(),
        },
    }
}

impl Rig {
    fn down(&mut self, x: f64, y: f64) {
        self.it.down(&mut self.d, x, y, SnapMode::Off, false, false);
    }
    fn down_with(&mut self, x: f64, y: f64, mode: SnapMode, shift: bool, modifier: bool) {
        self.it.down(&mut self.d, x, y, mode, shift, modifier);
    }
    fn mv(&mut self, x: f64, y: f64) {
        self.it.move_to(&mut self.d, x, y, SnapMode::Off, false);
    }
    fn mv_with(&mut self, x: f64, y: f64, mode: SnapMode, shift: bool) {
        self.it.move_to(&mut self.d, x, y, mode, shift);
    }
    fn up(&mut self, x: f64, y: f64) {
        self.it.up(&mut self.d, x, y);
    }
    fn cancel(&mut self) -> bool {
        self.it.cancel(&mut self.d)
    }
    fn ghost(&self) -> Option<Box<dyn Drawing>> {
        self.it.ghost(&self.d.registry)
    }
    fn create(&self) -> Option<&SerializedDrawing> {
        self.d.intents.iter().find_map(|i| match i {
            DrawingIntent::Create { doc } => Some(doc),
            _ => None,
        })
    }
    fn edit(&self) -> Option<&SerializedDrawing> {
        self.d.intents.iter().find_map(|i| match i {
            DrawingIntent::Edit { doc } => Some(doc),
            _ => None,
        })
    }
    fn kinds(&self) -> Vec<&'static str> {
        self.d.intents.iter().map(|i| i.kind()).collect()
    }
    fn has(&self, kind: &str) -> bool {
        self.kinds().contains(&kind)
    }
    fn select(&mut self, ids: &[&str]) {
        self.d.selected = ids.iter().map(|s| s.to_string()).collect();
    }
    fn anchors_of(&self, id: &str) -> Vec<DrawingPoint> {
        self.d
            .drawings
            .iter()
            .find(|d| d.base().id == id)
            .map(|d| d.base().anchors.clone())
            .unwrap_or_default()
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

// ── placing ──

#[test]
fn a_two_anchor_trend_line_places_over_two_clicks_and_a_moving_ghost() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0); // p1 -> price 10
    assert!(h.it.is_placing());
    assert!(h.ghost().is_none()); // no cursor yet
    h.mv(40.0, 60.0); // cursor -> ghost appears
    assert!(h.ghost().is_some());
    h.down(40.0, 60.0); // p2 -> finalize
    assert!(!h.it.is_placing());
    let create = h.create().expect("a create intent");
    assert_eq!(create.type_key, "trendline");
    assert_eq!(create.anchors.len(), 2);
    assert_eq!(create.anchors[0], pt(10.0, 10.0));
    assert!(h.has("tool-finished"));
}

#[test]
fn the_placement_ghost_previews_the_last_used_style_not_the_type_default() {
    let mut h = rig(Some("trendline"), vec![]);
    h.d.defaults = Some(
        serde_json::from_value(json!({
            "style": { "lineColor": "#ff0000", "lineWidth": 4, "lineStyle": "dashed" }
        }))
        .unwrap(),
    );
    h.down(10.0, 90.0);
    h.mv(40.0, 60.0);
    let ghost = h.ghost().expect("a ghost");
    let style = &ghost.base().style;
    assert_eq!(style.line_color, "#ff0000");
    assert_eq!(style.line_width, 4.0);
    assert_eq!(style.line_style, crate::model::LineStyle::Dashed);
}

#[test]
fn the_placement_ghost_previews_a_tools_remembered_levels_too_not_just_its_colors() {
    let mut h = rig(Some("fibretracement"), vec![]);
    let levels = json!([
        { "ratio": 0, "color": "#111111", "enabled": true },
        { "ratio": 0.705, "color": "#222222", "enabled": true },
        { "ratio": 1, "color": "#333333", "enabled": true },
    ]);
    h.d.defaults = Some(
        serde_json::from_value(json!({ "props": { "levels": levels, "reverse": true } })).unwrap(),
    );
    h.down(10.0, 90.0);
    h.mv(40.0, 60.0);
    let ghost = h.ghost().expect("a ghost");
    let props = ghost.write_props().expect("fib props");
    let ratios: Vec<f64> = props["levels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["ratio"].as_f64().unwrap())
        .collect();
    assert_eq!(ratios, vec![0.0, 0.705, 1.0]);
    assert_eq!(props["reverse"], json!(true));
}

#[test]
fn falls_back_to_the_type_default_color_when_no_last_used_style_exists() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    h.mv(40.0, 60.0);
    let ghost = h.ghost().expect("a ghost");
    assert_eq!(ghost.base().style.line_color, DEFAULT_DRAWING_COLOR);
}

#[test]
fn exposes_control_circle_markers_for_the_points_placed_so_far_while_placing() {
    let mut h = rig(Some("pitchfork"), vec![]);
    assert_eq!(h.it.placing_markers(&FakeProj), None); // nothing placed yet
    h.down(10.0, 90.0); // the pivot is visible before the shape can render
    assert_eq!(h.it.placing_markers(&FakeProj), Some(vec![(10.0, 90.0)]));
    h.mv(40.0, 60.0);
    h.down(40.0, 60.0);
    assert_eq!(
        h.it.placing_markers(&FakeProj),
        Some(vec![(10.0, 90.0), (40.0, 60.0)])
    );
    h.mv(40.0, 20.0);
    h.down(40.0, 20.0); // the third anchor finalizes: no longer placing
    assert_eq!(h.it.placing_markers(&FakeProj), None);
}

#[test]
fn a_one_anchor_hline_finalizes_on_the_first_click() {
    let mut h = rig(Some("hline"), vec![]);
    h.down(50.0, 70.0); // price 30
    assert!(!h.it.is_placing());
    assert_eq!(h.create().expect("create").anchors.len(), 1);
}

#[test]
fn parallel_channel_width_tracks_the_cursor_after_the_sloped_baseline_is_set() {
    let mut h = rig(Some("parallelchannel"), vec![]);
    h.down(0.0, 100.0); // p1 -> {0, 0}
    h.mv(50.0, 50.0);
    h.down(50.0, 50.0); // p2 -> {50, 50}: a sloped baseline
    assert!(h.it.is_placing());

    // Cursor ON the baseline: the channel stays collapsed whatever the cursor's time.
    h.mv(50.0, 50.0);
    assert_eq!(h.ghost().unwrap().base().anchors[2], pt(25.0, 25.0));
    h.mv(10.0, 90.0); // still on the baseline, but near p1
    assert_eq!(h.ghost().unwrap().base().anchors[2], pt(25.0, 25.0)); // no jump

    // 8 above the baseline near p2: the parallel line sits 8 above.
    h.mv(50.0, 42.0);
    assert_eq!(h.ghost().unwrap().base().anchors[2], pt(25.0, 33.0));

    h.down(50.0, 42.0); // commit
    assert!(!h.it.is_placing());
    assert_eq!(
        h.create().expect("create").anchors,
        vec![pt(0.0, 0.0), pt(50.0, 50.0), pt(25.0, 33.0)]
    );
}

#[test]
fn escape_cancels_an_in_progress_placement() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    assert!(h.cancel());
    assert!(!h.it.is_placing());
    assert!(h.has("tool-finished"));
    assert!(!h.has("create"));
}

#[test]
fn strong_magnet_snaps_placed_anchors_and_exposes_a_ring_marker() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down_with(13.0, 88.0, SnapMode::Strong, false, false); // raw {13, 12} -> {10, 10}
    h.mv_with(47.0, 64.0, SnapMode::Strong, false); // raw {47, 36} -> {50, 40}
    assert_eq!(
        h.it.snap_marker(),
        Some(&SnapMarker {
            point: pt(50.0, 40.0),
            pane_id: "price".into()
        })
    );
    h.down_with(47.0, 64.0, SnapMode::Strong, false, false);
    assert_eq!(
        h.create().expect("create").anchors,
        vec![pt(10.0, 10.0), pt(50.0, 40.0)]
    );
    assert_eq!(h.it.snap_marker(), None); // cleared on finalize
}

#[test]
fn off_magnet_never_snaps_and_shows_no_ring() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(13.0, 88.0);
    h.mv(47.0, 64.0);
    assert_eq!(h.it.snap_marker(), None);
}

#[test]
fn weak_magnet_snaps_only_within_the_cursor_radius() {
    let mut near = rig(Some("trendline"), vec![]);
    near.down_with(11.0, 89.0, SnapMode::Weak, false, false); // ~1.4px from the grid point
    near.mv_with(11.0, 89.0, SnapMode::Weak, false);
    assert_eq!(
        near.it.snap_marker(),
        Some(&SnapMarker {
            point: pt(10.0, 10.0),
            pane_id: "price".into()
        })
    );

    let mut far = rig(Some("trendline"), vec![]);
    far.mv_with(15.0, 85.0, SnapMode::Weak, false); // ~7px away: no snap, no ring
    assert_eq!(far.it.snap_marker(), None);
}

// ── snapCursor ──

#[test]
fn snap_cursor_strong_returns_the_snapped_pixel_and_sets_the_ring() {
    let mut h = rig(None, vec![]);
    let px = h.it.snap_cursor(&h.d, 13.0, 88.0, SnapMode::Strong);
    assert_eq!(px, (10.0, 90.0));
    assert_eq!(h.it.snap_marker().map(|m| m.point), Some(pt(10.0, 10.0)));
}

#[test]
fn snap_cursor_off_returns_the_raw_pixel_and_shows_no_ring() {
    let mut h = rig(None, vec![]);
    assert_eq!(
        h.it.snap_cursor(&h.d, 13.0, 88.0, SnapMode::Off),
        (13.0, 88.0)
    );
    assert_eq!(h.it.snap_marker(), None);
}

#[test]
fn snap_cursor_weak_snaps_only_within_the_cursor_radius() {
    let mut near = rig(None, vec![]);
    assert_eq!(
        near.it.snap_cursor(&near.d, 11.0, 89.0, SnapMode::Weak),
        (10.0, 90.0)
    );
    assert!(near.it.snap_marker().is_some());
    let mut far = rig(None, vec![]);
    assert_eq!(
        far.it.snap_cursor(&far.d, 15.0, 85.0, SnapMode::Weak),
        (15.0, 85.0)
    );
    assert_eq!(far.it.snap_marker(), None);
}

#[test]
fn clear_snap_marker_drops_the_ring() {
    let mut h = rig(None, vec![]);
    h.it.snap_cursor(&h.d, 13.0, 88.0, SnapMode::Strong);
    assert!(h.it.snap_marker().is_some());
    h.it.clear_snap_marker();
    assert_eq!(h.it.snap_marker(), None);
}

// ── Shift angle snap ──

#[test]
fn shift_while_placing_locks_the_ghost_to_the_nearest_45_degree_ray() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0); // p1 -> {10, 10}
    h.mv_with(50.0, 85.0, SnapMode::Off, true); // ~7 degrees off horizontal -> flat
    let ghost = h.ghost().unwrap();
    let p2 = ghost.base().anchors[1];
    assert!(close(p2.price, 10.0));
    assert!(close(p2.time, 10.0 + 40.0f64.hypot(5.0)));
}

#[test]
fn a_diagonal_cursor_snaps_to_the_45_degree_ray() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    h.mv_with(45.0, 60.0, SnapMode::Off, true); // ~40.6 degrees up -> 45
    let p2 = h.ghost().unwrap().base().anchors[1];
    assert!(close(p2.time - 10.0, p2.price - 10.0));
    assert!(p2.price > 10.0);
}

#[test]
fn a_shift_click_commits_the_snapped_anchor_and_bypasses_the_magnet() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    h.mv_with(47.0, 92.0, SnapMode::Strong, true); // shift: magnet skipped, no ring
    assert_eq!(h.it.snap_marker(), None);
    h.down_with(47.0, 92.0, SnapMode::Strong, true, false); // near-horizontal -> flat at p1
    assert!(close(h.create().expect("create").anchors[1].price, 10.0));
}

#[test]
fn shift_while_dragging_a_line_endpoint_relocks_its_angle_around_the_other_anchor() {
    let d = make("trendline", "dw-1", &[(10.0, 10.0), (50.0, 50.0)]);
    let mut h = rig(None, vec![d]);
    h.d.hovered = Some("dw-1".into());
    h.down(50.0, 50.0); // grab handle p2
    h.mv_with(90.0, 85.0, SnapMode::Off, true); // ~4 degrees below horizontal -> flat
    h.up(90.0, 85.0);
    let edit = h.edit().expect("an edit");
    assert_eq!(edit.anchors[0], pt(10.0, 10.0)); // pivot untouched
    assert!(close(edit.anchors[1].price, 10.0));
}

#[test]
fn moving_ids_names_the_dragged_drawing_and_its_riders_only_past_the_slop() {
    let a = make("hline", "a", &[(10.0, 30.0)]);
    let b = make("hline", "b", &[(10.0, 50.0)]);
    let mut h = rig(None, vec![a, b]);
    h.select(&["a", "b"]);
    h.down(40.0, 70.0); // a's body (y = 100 - 30)
    assert!(h.it.moving_ids().is_empty()); // pressed, not yet moved
    h.mv(60.0, 80.0);
    let mut moving: Vec<String> = h.it.moving_ids().into_iter().collect();
    moving.sort();
    assert_eq!(moving, vec!["a", "b"]); // the selection rides along
    h.up(60.0, 80.0);
    assert!(h.it.moving_ids().is_empty());
    // A Ctrl-drag moves copies: the store drawings stay put, so nothing is "moving".
    h.down_with(40.0, 70.0, SnapMode::Off, false, true);
    h.mv(60.0, 80.0);
    assert!(h.it.moving_ids().is_empty());
    h.up(60.0, 80.0);
}

#[test]
fn shift_leaves_non_line_tools_alone() {
    let mut h = rig(Some("datepricerange"), vec![]);
    h.down(10.0, 90.0);
    h.mv_with(40.0, 80.0, SnapMode::Off, true); // not a segment tool: no angle lock
    assert_eq!(h.ghost().unwrap().base().anchors[1], pt(40.0, 20.0));
}

// ── selection and claim ──

fn hline() -> Box<dyn Drawing> {
    make("hline", "dw-1", &[(10.0, 30.0)])
}

#[test]
fn claims_when_armed_when_placing_or_over_a_drawing_and_yields_on_empty_space() {
    let armed = rig(Some("trendline"), vec![]);
    assert!(armed.it.claim(&armed.d, 5.0, 5.0)); // armed: always

    let idle = rig(None, vec![hline()]); // hline at y = 70
    assert!(idle.it.claim(&idle.d, 50.0, 70.0)); // over the drawing
    assert!(!idle.it.claim(&idle.d, 50.0, 10.0)); // empty space: pan
}

#[test]
fn a_click_on_a_drawing_opens_its_settings_and_an_empty_press_does_nothing() {
    let mut h = rig(None, vec![hline()]);
    h.down(50.0, 70.0);
    h.up(50.0, 70.0); // click (no move) on the hline
    assert_eq!(h.d.settings, vec![("dw-1".to_string(), 50.0, 70.0)]);
    h.down(50.0, 10.0);
    h.up(50.0, 10.0); // empty space: no press captured, no settings
    assert_eq!(h.d.settings.len(), 1);
}

#[test]
fn shift_click_toggles_selection_without_dragging_or_opening_settings() {
    let mut h = rig(None, vec![hline()]);
    h.down_with(50.0, 70.0, SnapMode::Off, true, false);
    assert_eq!(
        h.d.intents,
        vec![DrawingIntent::Select {
            ids: vec!["dw-1".into()],
            additive: true
        }]
    );
    h.up(50.0, 70.0);
    assert!(h.d.settings.is_empty()); // shift-click is selection-only
}

#[test]
fn ctrl_click_toggles_selection_on_release_without_opening_settings() {
    let mut h = rig(None, vec![hline()]);
    h.down_with(50.0, 70.0, SnapMode::Strong, false, true);
    assert!(h.d.intents.is_empty()); // nothing until the release decides click vs drag
    h.up(51.0, 71.0); // within the slop: a click
    assert_eq!(
        h.d.intents,
        vec![DrawingIntent::Select {
            ids: vec!["dw-1".into()],
            additive: true
        }]
    );
    assert!(h.d.settings.is_empty());
}

// ── marquee ──

fn inside() -> Box<dyn Drawing> {
    make("trendline", "dw-1", &[(20.0, 80.0), (40.0, 60.0)])
}
fn outside() -> Box<dyn Drawing> {
    make("trendline", "dw-2", &[(150.0, 20.0), (180.0, 10.0)])
}
fn crossing() -> Box<dyn Drawing> {
    make("hline", "dw-3", &[(0.0, 70.0)])
}

fn select_ids(ids: &[&str]) -> DrawingIntent {
    DrawingIntent::Select {
        ids: ids.iter().map(|s| s.to_string()).collect(),
        additive: false,
    }
}

#[test]
fn marquee_starts_only_when_idle_with_no_tool_armed_and_claims_the_gesture() {
    let mut armed = rig(Some("trendline"), vec![]);
    assert!(!armed.it.begin_marquee(&armed.d, 5.0, 5.0));
    let mut h = rig(None, vec![inside()]);
    assert!(!h.it.claim(&h.d, 100.0, 90.0)); // empty space: the host decides
    assert!(h.it.begin_marquee(&h.d, 100.0, 90.0));
    assert!(h.it.claim(&h.d, 100.0, 90.0)); // in flight: the layer owns the moves
    assert_eq!(
        h.it.marquee_rect(),
        Some(Bounds {
            x: 100.0,
            y: 90.0,
            w: 0.0,
            h: 0.0
        })
    );
}

#[test]
fn marquee_release_selects_every_visible_drawing_whose_bounds_touch_the_box() {
    let mut h = rig(None, vec![inside(), outside(), crossing()]);
    h.it.begin_marquee(&h.d, 10.0, 10.0);
    h.mv(60.0, 50.0);
    assert_eq!(
        h.it.marquee_rect(),
        Some(Bounds {
            x: 10.0,
            y: 10.0,
            w: 50.0,
            h: 40.0
        })
    );
    h.up(60.0, 50.0);
    assert_eq!(h.it.marquee_rect(), None);
    assert_eq!(h.d.intents, vec![select_ids(&["dw-1", "dw-3"])]); // dw-2 sits far right
}

#[test]
fn a_sweep_dragged_up_left_normalizes_and_hidden_drawings_are_never_picked() {
    let mut hidden = inside();
    hidden.base_mut().visible = false;
    let mut h = rig(None, vec![hidden, crossing()]);
    h.it.begin_marquee(&h.d, 60.0, 50.0);
    h.mv(10.0, 10.0);
    h.up(10.0, 10.0);
    assert_eq!(h.d.intents, vec![select_ids(&["dw-3"])]);
}

#[test]
fn a_marquee_adds_to_the_existing_selection_and_stays_silent_when_nothing_new_is_touched() {
    let mut h = rig(None, vec![inside(), outside()]);
    h.select(&["dw-2"]);
    h.it.begin_marquee(&h.d, 10.0, 10.0);
    h.mv(60.0, 50.0);
    h.up(60.0, 50.0);
    assert_eq!(h.d.intents, vec![select_ids(&["dw-2", "dw-1"])]); // dw-2 kept, dw-1 added
    h.d.intents.clear();
    h.select(&["dw-1", "dw-2"]);
    h.it.begin_marquee(&h.d, 10.0, 10.0);
    h.mv(60.0, 50.0);
    h.up(60.0, 50.0); // both already selected: no intent
    assert!(h.d.intents.is_empty());
}

#[test]
fn a_marquee_within_the_slop_selects_nothing_and_escape_cancels_a_sweep() {
    let mut h = rig(None, vec![inside()]);
    h.it.begin_marquee(&h.d, 30.0, 30.0);
    h.mv(32.0, 31.0);
    h.up(32.0, 31.0); // a twitch, not a sweep, even though the box touches dw-1
    assert!(h.d.intents.is_empty());
    h.it.begin_marquee(&h.d, 10.0, 10.0);
    h.mv(60.0, 50.0);
    assert!(h.cancel());
    assert_eq!(h.it.marquee_rect(), None);
    h.up(60.0, 50.0);
    assert!(h.d.intents.is_empty());
}

// ── click-move-click placement ──

#[test]
fn a_measurement_box_is_placed_click_move_click() {
    let mut h = rig(Some("datepricerange"), vec![]);
    h.down(10.0, 90.0); // the first corner
    h.mv(40.0, 60.0); // live preview
    assert!(h.it.is_placing());
    assert!(!h.has("create")); // nothing until the second click
    h.down(40.0, 60.0); // the opposite corner finalizes
    let create = h.create().expect("create");
    assert_eq!(create.type_key, "datepricerange");
    assert_eq!(create.anchors.len(), 2);
}

#[test]
fn a_long_position_click_move_click_has_three_anchors_with_the_target_above_the_entry() {
    let mut h = rig(Some("position"), vec![]);
    h.down(20.0, 60.0); // entry (price 40)
    h.mv(40.0, 40.0);
    h.down(40.0, 40.0); // target above the entry (price 60): long
    let a = &h.create().expect("create").anchors;
    assert_eq!(a.len(), 3);
    let (entry, stop, target) = (a[0], a[1], a[2]);
    assert!(target.price > entry.price);
    assert!(stop.price < entry.price);
}

#[test]
fn placing_the_target_the_other_way_flips_to_short() {
    let mut h = rig(Some("position"), vec![]);
    h.down(20.0, 60.0);
    h.mv(40.0, 80.0);
    h.down(40.0, 80.0); // target below the entry: short
    let a = &h.create().expect("create").anchors;
    assert!(a[2].price < a[0].price);
    assert!(a[1].price > a[0].price);
}

#[test]
fn a_degenerate_second_click_drops_a_default_position_box() {
    let mut h = rig(Some("position"), vec![]);
    h.down(30.0, 50.0);
    h.down(30.0, 50.0); // second click on the entry: default R:R box
    assert_eq!(h.create().expect("create").anchors.len(), 3);
}

#[test]
fn a_single_click_leaves_a_range_placing() {
    let mut h = rig(Some("datepricerange"), vec![]);
    h.down(10.0, 90.0);
    assert!(h.it.is_placing());
    assert!(!h.has("create"));
}

// ── variable and freehand placement ──

#[test]
fn polyline_clicks_add_vertices_and_a_double_click_drops_the_dup_and_finalizes() {
    let mut h = rig(Some("polyline"), vec![]);
    h.down(10.0, 90.0);
    h.mv(40.0, 60.0);
    h.down(40.0, 60.0);
    h.mv(70.0, 90.0);
    h.down(70.0, 90.0);
    assert!(h.it.is_placing()); // variable: keeps going
    h.down(70.0, 90.0); // the second click of a double-click (duplicate)
    assert!(h.it.finish_placing(&mut h.d, true));
    let create = h.create().expect("create");
    assert_eq!(create.type_key, "polyline");
    assert_eq!(create.anchors.len(), 3); // the duplicate was dropped
}

#[test]
fn freehand_press_and_drag_captures_a_path_and_release_finalizes() {
    let mut h = rig(Some("freehand"), vec![]);
    h.down(10.0, 90.0);
    h.mv(30.0, 70.0);
    h.mv(50.0, 50.0);
    h.mv(70.0, 30.0);
    assert!(h.it.is_placing());
    h.up(70.0, 30.0);
    let create = h.create().expect("create");
    assert_eq!(create.type_key, "freehand");
    assert!(create.anchors.len() >= 3);
}

#[test]
fn a_marathon_freehand_stroke_keeps_capturing_past_the_path_cap() {
    let mut h = rig(Some("freehand"), vec![]);
    h.down(0.0, 50.0);
    let samples = MAX_PATH_POINTS + 200; // enough 5px steps to overflow the cap
    for i in 1..=samples {
        h.mv(i as f64 * 5.0, 50.0);
    }
    h.up(samples as f64 * 5.0, 50.0);
    let create = h.create().expect("create");
    assert!(create.anchors.len() <= MAX_PATH_POINTS);
    // The tail of the stroke, drawn AFTER the cap was first hit, made it into the path.
    let last = create.anchors.last().unwrap();
    assert!(last.time > MAX_PATH_POINTS as f64 * 5.0);
}

#[test]
fn freehand_a_bare_click_with_no_drag_keeps_nothing() {
    let mut h = rig(Some("freehand"), vec![]);
    h.down(10.0, 90.0);
    h.up(10.0, 90.0);
    assert!(!h.has("create"));
    assert!(h.has("tool-finished"));
}

// ── dragging ──

fn trend() -> Box<dyn Drawing> {
    make("trendline", "dw-1", &[(10.0, 10.0), (50.0, 50.0)])
}
fn hline2() -> Box<dyn Drawing> {
    make("hline", "dw-2", &[(10.0, 30.0)])
}

#[test]
fn a_press_that_moves_past_the_slop_becomes_a_whole_body_drag() {
    let mut h = rig(None, vec![trend()]);
    h.down(30.0, 70.0); // press the body: not a drag yet
    assert!(!h.it.is_dragging());
    h.mv(40.0, 65.0); // dt = +10, dp = +5
    assert!(h.it.is_dragging());
    h.up(40.0, 65.0);
    assert_eq!(
        h.edit().expect("edit").anchors,
        vec![pt(20.0, 15.0), pt(60.0, 55.0)]
    );
    assert!(h.d.settings.is_empty()); // a drag does NOT open settings
}

#[test]
fn a_handle_drag_moves_only_the_grabbed_anchor() {
    let mut h = rig(None, vec![trend()]);
    h.d.hovered = Some("dw-1".into()); // hovering shows handles: its handle is grabbable
    h.down(10.0, 90.0); // grab handle p1
    h.mv(15.0, 85.0);
    h.up(15.0, 85.0);
    let edit = h.edit().expect("edit");
    assert_eq!(edit.anchors[0], pt(15.0, 15.0));
    assert_eq!(edit.anchors[1], pt(50.0, 50.0));
}

#[test]
fn grabs_an_off_body_handle_of_a_selected_drawing() {
    // ellipse box (0,0)-(40,40): the corner handle sits at px (40, 60), OUTSIDE the curve
    let e = make("ellipse", "dw-9", &[(0.0, 0.0), (40.0, 40.0)]);
    let mut h = rig(None, vec![e]);
    h.select(&["dw-9"]); // selected (popup open) but NOT hovered
    h.down(40.0, 60.0);
    h.mv(45.0, 55.0);
    h.up(45.0, 55.0);
    assert_eq!(h.edit().expect("edit").anchors[1], pt(45.0, 45.0));
}

#[test]
fn a_y_only_handle_ignores_horizontal_motion() {
    let mut h = rig(None, vec![hline2()]);
    h.d.hovered = Some("dw-2".into());
    h.down(10.0, 70.0); // the hline's handle (price 30 -> y 70)
    h.mv(40.0, 60.0); // would change time, but free = y
    h.up(40.0, 60.0);
    assert_eq!(h.edit().expect("edit").anchors[0], pt(10.0, 40.0));
}

#[test]
fn a_locked_drawing_opens_settings_on_click_but_never_drags() {
    let mut d = trend();
    d.base_mut().locked = true;
    let mut h = rig(None, vec![d]);
    h.down(30.0, 70.0);
    h.mv(40.0, 65.0); // locked: ignored, stays a click
    assert!(!h.it.is_dragging());
    h.up(40.0, 65.0);
    assert!(!h.has("edit")); // never moved
    assert_eq!(h.d.settings, vec![("dw-1".to_string(), 40.0, 65.0)]);
}

#[test]
fn escape_during_a_drag_restores_the_original_anchors() {
    let mut h = rig(None, vec![trend()]);
    h.down(30.0, 70.0);
    h.mv(40.0, 65.0);
    assert!(h.cancel());
    assert_eq!(h.anchors_of("dw-1"), vec![pt(10.0, 10.0), pt(50.0, 50.0)]);
    assert!(!h.has("edit"));
}

#[test]
fn body_dragging_a_multi_selected_drawing_moves_the_whole_selection_as_one_edit_many() {
    let mut locked = make("hline", "dw-3", &[(10.0, 80.0)]);
    locked.base_mut().locked = true;
    let mut h = rig(None, vec![trend(), hline2(), locked]);
    h.select(&["dw-1", "dw-2", "dw-3"]);
    h.down(20.0, 80.0); // the trend line's body, clear of the hline at y = 70
    h.mv(30.0, 75.0); // dt = +10, dp = +5
    assert_eq!(h.anchors_of("dw-2"), vec![pt(20.0, 35.0)]); // the rider follows live
    assert_eq!(h.anchors_of("dw-3"), vec![pt(10.0, 80.0)]); // a locked member stays put
    h.up(30.0, 75.0);
    let many = h.d.intents.iter().find_map(|i| match i {
        DrawingIntent::EditMany { docs } => Some(docs),
        _ => None,
    });
    let many = many.expect("an edit-many");
    assert_eq!(
        many.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        vec!["dw-1", "dw-2"]
    );
    assert_eq!(many[0].anchors, vec![pt(20.0, 15.0), pt(60.0, 55.0)]);
    assert!(!h.has("edit"));
}

#[test]
fn a_handle_drag_of_a_multi_selected_drawing_reshapes_only_that_one() {
    let mut h = rig(None, vec![trend(), hline2()]);
    h.select(&["dw-1", "dw-2"]);
    h.down(10.0, 90.0); // the trend line's p1 handle
    h.mv(15.0, 85.0);
    h.up(15.0, 85.0);
    assert_eq!(h.anchors_of("dw-2"), vec![pt(10.0, 30.0)]);
    assert_eq!(h.kinds(), vec!["edit"]);
}

#[test]
fn escape_during_a_group_drag_restores_every_member() {
    let mut h = rig(None, vec![trend(), hline2()]);
    h.select(&["dw-1", "dw-2"]);
    h.down(20.0, 80.0);
    h.mv(30.0, 75.0);
    assert!(h.cancel());
    assert_eq!(h.anchors_of("dw-1"), vec![pt(10.0, 10.0), pt(50.0, 50.0)]);
    assert_eq!(h.anchors_of("dw-2"), vec![pt(10.0, 30.0)]);
    assert!(h.d.intents.is_empty());
}

// ── Ctrl/Cmd-drag duplicates ──

#[test]
fn a_ctrl_drag_of_the_body_moves_a_copy_and_commits_it_on_release() {
    let mut h = rig(None, vec![trend()]);
    h.down_with(30.0, 70.0, SnapMode::Strong, false, true);
    assert!(h.it.drag_clones().is_none()); // nothing is copied until the press becomes a drag
    h.mv(40.0, 65.0);
    {
        let clones = h.it.drag_clones().expect("clones");
        assert_eq!(clones.len(), 1);
        assert_eq!(
            clones[0].base().anchors,
            vec![pt(20.0, 15.0), pt(60.0, 55.0)]
        ); // the copy follows
    }
    assert_eq!(h.anchors_of("dw-1"), vec![pt(10.0, 10.0), pt(50.0, 50.0)]); // the source stays
    h.up(40.0, 65.0);
    assert!(h.it.drag_clones().is_none());
    assert_eq!(h.d.intents.len(), 1);
    let DrawingIntent::Clone { docs } = &h.d.intents[0] else {
        panic!("expected a clone intent");
    };
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].type_key, "trendline");
    assert_eq!(docs[0].anchors, vec![pt(20.0, 15.0), pt(60.0, 55.0)]);
    assert!(h.d.settings.is_empty());
}

#[test]
fn ctrl_dragging_a_member_of_a_multi_selection_copies_the_whole_selection() {
    let mut h = rig(None, vec![trend(), hline2()]);
    h.select(&["dw-1", "dw-2"]);
    h.down_with(20.0, 80.0, SnapMode::Off, false, true);
    h.mv(30.0, 75.0);
    h.up(30.0, 75.0);
    let docs =
        h.d.intents
            .iter()
            .find_map(|i| match i {
                DrawingIntent::Clone { docs } => Some(docs),
                _ => None,
            })
            .expect("a clone intent");
    assert_eq!(
        docs.iter().map(|d| d.type_key.as_str()).collect::<Vec<_>>(),
        vec!["trendline", "hline"]
    );
    assert_eq!(docs[1].anchors, vec![pt(20.0, 35.0)]);
    assert_eq!(h.anchors_of("dw-2"), vec![pt(10.0, 30.0)]); // sources untouched
}

#[test]
fn ctrl_on_a_handle_keeps_its_resize_meaning() {
    let mut h = rig(None, vec![trend()]);
    h.d.hovered = Some("dw-1".into());
    h.down_with(10.0, 90.0, SnapMode::Off, false, true); // grab p1 with Ctrl held
    h.mv(15.0, 85.0);
    assert!(h.it.drag_clones().is_none());
    h.up(15.0, 85.0);
    assert_eq!(h.kinds(), vec!["edit"]);
    assert_eq!(h.anchors_of("dw-1")[0], pt(15.0, 15.0));
}

#[test]
fn escape_during_a_ctrl_drag_leaves_nothing_behind_and_the_source_untouched() {
    let mut h = rig(None, vec![trend()]);
    h.down_with(30.0, 70.0, SnapMode::Off, false, true);
    h.mv(40.0, 65.0);
    assert!(h.cancel());
    assert!(h.it.drag_clones().is_none());
    assert_eq!(h.anchors_of("dw-1"), vec![pt(10.0, 10.0), pt(50.0, 50.0)]);
    h.up(40.0, 65.0);
    assert!(h.d.intents.is_empty());
}

#[test]
fn a_locked_drawing_is_never_copied_by_a_ctrl_drag() {
    let mut d = trend();
    d.base_mut().locked = true;
    let mut h = rig(None, vec![d]);
    h.down_with(30.0, 70.0, SnapMode::Off, false, true);
    h.mv(40.0, 65.0);
    assert!(h.it.drag_clones().is_none());
    h.up(40.0, 65.0);
    assert_eq!(
        h.d.intents,
        vec![DrawingIntent::Select {
            ids: vec!["dw-1".into()],
            additive: true
        }]
    );
}

// ── rectangle handles ──

fn rect_box() -> Box<dyn Drawing> {
    make("box", "dw-1", &[(10.0, 10.0), (50.0, 50.0)])
}

/// Press at `from`, drag to `to`, release; the anchors of the committed edit.
fn drag(from: (f64, f64), to: (f64, f64)) -> Option<Vec<DrawingPoint>> {
    let mut h = rig(None, vec![rect_box()]);
    h.d.hovered = Some("dw-1".into());
    h.select(&["dw-1"]);
    h.down(from.0, from.1);
    h.mv(to.0, to.1);
    h.up(to.0, to.1);
    h.edit().map(|d| d.anchors.clone())
}

#[test]
fn a_rectangle_shows_eight_handles() {
    let pts = rect_box().handle_points(&FakeProj);
    assert_eq!(pts.len(), 8);
    for p in [
        (10.0, 90.0),
        (50.0, 50.0),
        (50.0, 90.0),
        (10.0, 50.0),
        (30.0, 90.0),
        (50.0, 70.0),
        (30.0, 50.0),
        (10.0, 70.0),
    ] {
        assert!(pts.contains(&p), "{p:?} missing from {pts:?}");
    }
}

#[test]
fn dragging_the_middle_of_the_top_side_moves_only_the_top() {
    assert_eq!(
        drag((30.0, 50.0), (42.0, 35.0)),
        Some(vec![pt(10.0, 10.0), pt(50.0, 65.0)])
    );
}

#[test]
fn dragging_the_middle_of_the_left_side_moves_only_the_left_side() {
    assert_eq!(
        drag((10.0, 70.0), (0.0, 62.0)),
        Some(vec![pt(0.0, 10.0), pt(50.0, 50.0)])
    );
}

#[test]
fn dragging_one_of_the_two_added_corners_resizes_both_of_its_sides() {
    assert_eq!(
        drag((50.0, 90.0), (65.0, 95.0)),
        Some(vec![pt(10.0, 5.0), pt(65.0, 50.0)])
    );
}

#[test]
fn dragging_an_original_corner_still_moves_that_corner_freely() {
    assert_eq!(
        drag((10.0, 90.0), (4.0, 96.0)),
        Some(vec![pt(4.0, 4.0), pt(50.0, 50.0)])
    );
}

// ── edge cases the original tests miss ──

#[test]
fn an_unknown_tool_places_nothing() {
    let mut h = rig(Some("nonsense"), vec![]);
    h.down(10.0, 10.0);
    assert!(!h.it.is_placing());
    assert!(h.d.intents.is_empty());
}

#[test]
fn a_drag_placed_box_finalizes_on_release_with_the_cursor_as_the_far_corner() {
    let mut h = rig(Some("magnifier"), vec![]);
    h.down(10.0, 90.0);
    assert!(h.it.is_placing()); // drag placement waits for the release
    h.mv(60.0, 40.0);
    h.up(60.0, 40.0);
    assert_eq!(
        h.create().expect("create").anchors,
        vec![pt(10.0, 10.0), pt(60.0, 60.0)]
    );
}

#[test]
fn a_bare_click_with_a_drag_tool_discards_the_placement() {
    let mut h = rig(Some("magnifier"), vec![]);
    h.down(10.0, 90.0);
    h.up(10.0, 90.0);
    assert!(!h.has("create"));
    assert!(h.has("tool-finished"));
    assert!(!h.it.is_placing());
}

#[test]
fn clearing_the_tool_externally_drops_a_placement() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    h.it.on_tool_cleared(&mut h.d);
    assert!(!h.it.is_placing());
}

#[test]
fn an_hline_ghost_shows_before_any_cursor_move() {
    let mut h = rig(Some("trendline"), vec![]);
    h.down(10.0, 90.0);
    assert!(h.ghost().is_none());
    assert_eq!(h.cursor_hint(50.0, 5.0), None); // armed: the crosshair stays
}

impl Rig {
    fn cursor_hint(&self, x: f64, y: f64) -> Option<&'static str> {
        self.it.cursor_at(&self.d, x, y)
    }
}

#[test]
fn the_cursor_is_a_pointer_over_a_drawing_only_when_no_tool_is_armed() {
    let h = rig(None, vec![hline()]);
    assert_eq!(h.cursor_hint(50.0, 70.0), Some("pointer"));
    assert_eq!(h.cursor_hint(50.0, 10.0), None);
}

#[test]
fn a_drawing_on_a_pane_the_projector_lost_is_not_placed_in_the_marker_list() {
    let mut h = rig(Some("pitchfork"), vec![]);
    h.down(10.0, 90.0);
    if let State::Placing { draft, .. } = &mut h.it.state {
        draft.base_mut().pane_id = "gone".into();
    }
    assert_eq!(h.it.placing_markers(&FakeProj), None);
}
