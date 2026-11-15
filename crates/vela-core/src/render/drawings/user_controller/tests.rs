//! The user-drawings controller against a mounted [`NativeRenderer`] on a
//! [`TestHost`], driven the way the browser drives it (gestures in, the core's
//! `DrawingController` on the other side of the port, the host seam recording the DOM
//! work), plus the parts of the original tests that drive the renderer-side controller.

use std::rc::Rc;

use serde_json::json;

use super::*;
use crate::drawings::controller::{DrawingController, no_translator};
use crate::drawings::{DrawingRegistry, SerializedDrawing};
use crate::events::types::ChartEventBus;
use crate::model::{Ohlcv, Pane, PaneKind};
use crate::options::DrawingsOption;
use crate::ports::chart_renderer::{ChartRenderer, SetBarsOptions};
use crate::render::canvas::RecordingCanvas;
use crate::render::native::testing::{TestHost, line_model, sample_bars};
use crate::render::native::{CanvasSlot, NativeRenderer};
use crate::theme::dark_theme;

const T0: i64 = 1_700_000_000_000;
const STEP: i64 = 60_000;
const OFF: SnapMode = SnapMode::Off;

struct World {
    host: Rc<TestHost>,
    r: Rc<NativeRenderer>,
    ctl: Rc<UserDrawingController>,
    core: DrawingController,
    events: ChartEventBus,
    bars: Vec<Ohlcv>,
}

fn world_with(option: Option<DrawingsOption>) -> World {
    let host = TestHost::new(800.0, 420.0, 1.0);
    let r = NativeRenderer::new(host.clone(), None);
    r.mount(&dark_theme());
    let bars = sample_bars(200, T0, STEP);
    r.set_bars(&bars, SetBarsOptions::default());
    host.run_until_idle();
    let renderer: Rc<dyn ChartRenderer> = r.clone();
    let events = ChartEventBus::new();
    let core = DrawingController::new(
        renderer,
        events.clone(),
        Rc::new(DrawingRegistry::builtin()),
        option,
        None,
        no_translator(),
    );
    let ctl = r.user_drawings_controller().expect("attached by default");
    World {
        host,
        r,
        ctl,
        core,
        events,
        bars,
    }
}

fn world() -> World {
    world_with(None)
}

impl World {
    /// The pixel of bar `i` at `price` on the price pane.
    fn px(&self, i: usize, price: f64) -> (f64, f64) {
        let t = self.bars[i].time as f64;
        self.r.with_drawing_projector(|p| {
            (
                p.x_of(t),
                p.y_of(price, "price").expect("the price pane exists"),
            )
        })
    }

    fn idle(&self) {
        self.host.run_until_idle();
    }

    fn click(&self, p: (f64, f64)) {
        self.ctl.pointer_move(p.0, p.1, OFF, false, false);
        self.ctl.pointer_down(p.0, p.1, OFF, false, false);
        self.ctl.pointer_up(p.0, p.1, OFF);
    }

    fn drawings(&self) -> Vec<SerializedDrawing> {
        self.core.all()
    }

    fn drawings_canvas(&self) -> Rc<RecordingCanvas> {
        self.host.canvas(&CanvasSlot::Drawings).unwrap()
    }

    fn ui(&self) -> Vec<DrawingsUiEvent> {
        self.host.drawings_ui_events()
    }

    /// Place a trend line between two bars (the tool must be armed by the caller).
    fn place_trendline(&self) -> SerializedDrawing {
        let a = self.px(60, 110.0);
        let b = self.px(120, 120.0);
        self.core.set_tool(Some("trendline"));
        self.click(a);
        self.ctl.pointer_move(b.0, b.1, OFF, false, false);
        self.click(b);
        self.idle();
        self.drawings().pop().expect("the trend line was created")
    }
}

fn arcs_of(c: &RecordingCanvas, radius: f64) -> usize {
    c.calls("arc")
        .iter()
        .filter(|a| a.get(3).and_then(|r| r.as_f64()) == Some(radius))
        .count()
}

fn key(k: &str) -> DrawingKeyEvent {
    DrawingKeyEvent::new(k)
}

fn ctrl(k: &str) -> DrawingKeyEvent {
    DrawingKeyEvent {
        key: k.into(),
        ctrl_key: true,
        ..DrawingKeyEvent::default()
    }
}

fn popup_opens(events: &[DrawingsUiEvent]) -> Vec<PopupRequest> {
    events
        .iter()
        .filter_map(|e| match e {
            DrawingsUiEvent::Popup(PopupEvent::Open(r)) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn editor_events(events: &[DrawingsUiEvent]) -> Vec<TextEditorEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            DrawingsUiEvent::TextEditor(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

// ── the shell attaches it ──

#[test]
fn the_shell_attaches_the_controller_by_default_and_the_core_drives_its_port() {
    let w = world();
    assert!(w.r.user_drawings_port().is_some());
    assert!(w.core.supported());
    // The core pushed its toolbar definition and visibility through the port.
    let toolbars: Vec<ToolbarUiState> = w
        .ui()
        .into_iter()
        .filter_map(|e| match e {
            DrawingsUiEvent::Toolbar(t) => Some(t),
            _ => None,
        })
        .collect();
    assert!(!toolbars.is_empty());
    let last = toolbars.last().unwrap();
    assert!(last.definition.get("groups").is_some());
    assert!(last.visible);
    assert_eq!(last.gutter, TOOLBAR_WIDTH);
    assert_eq!(w.r.with(|st| st.toolbar_gutter), TOOLBAR_WIDTH);
}

#[test]
fn collapsing_the_toolbar_narrows_the_gutter_and_mobile_releases_it() {
    let w = world();
    w.ctl.toolbar_collapsed(true);
    assert_eq!(w.r.with(|st| st.toolbar_gutter), TOOLBAR_COLLAPSED_WIDTH);
    w.ctl.toolbar_collapsed(false);
    assert_eq!(w.r.with(|st| st.toolbar_gutter), TOOLBAR_WIDTH);
    w.r.set_layout_mode(LayoutMode::Mobile);
    w.idle(); // the hook runs under the renderer's borrow: the gutter follows on a frame
    assert_eq!(w.r.with(|st| st.toolbar_gutter), 0.0);
    w.r.set_layout_mode(LayoutMode::Desktop);
    w.idle();
    assert_eq!(w.r.with(|st| st.toolbar_gutter), TOOLBAR_WIDTH);
}

#[test]
fn arming_a_tool_reaches_the_controller_and_the_toolbar() {
    let w = world();
    w.core.set_tool(Some("trendline"));
    assert_eq!(w.ctl.active_tool().as_deref(), Some("trendline"));
    let Some(DrawingsUiEvent::Toolbar(t)) = w
        .ui()
        .into_iter()
        .rev()
        .find(|e| matches!(e, DrawingsUiEvent::Toolbar(_)))
    else {
        panic!("a toolbar snapshot");
    };
    assert_eq!(t.active_tool.as_deref(), Some("trendline"));
    // An armed tool claims every press, even on empty space.
    assert!(w.ctl.claim(5.0, 5.0));
    w.core.set_tool(None);
    assert!(!w.ctl.claim(5.0, 5.0));
}

// ── the end to end: place, select, drag, delete, undo ──

#[test]
fn a_trend_line_is_placed_selected_dragged_deleted_and_restored_by_undo() {
    let w = world();
    let line = w.place_trendline();
    assert_eq!(line.type_key, "trendline");
    assert_eq!(line.anchors.len(), 2);
    assert_eq!(w.core.get_tool(), None); // one-shot: back to the pointer
    assert_eq!(w.ctl.active_tool(), None);

    // The new drawing sits under the candles (interleaved): its body paints on a slice
    // surface behind the series, and the editing UI opened for it.
    assert!(
        w.r.with(|st| st.scene.drawing_slices.contains_key("price")),
        "a slice for the price pane"
    );
    let slice = w
        .host
        .slice_surfaces()
        .into_iter()
        .last()
        .expect("a slice surface");
    assert!(!slice.recording().calls("stroke").is_empty());
    assert_eq!(w.ctl.selected_ids(), vec![line.id.clone()]);
    let opens = popup_opens(&w.ui());
    assert_eq!(
        opens.last().map(|o| o.ids.clone()),
        Some(vec![line.id.clone()])
    );
    assert!(!opens.last().unwrap().multi);
    assert!(opens.last().unwrap().anchor.is_some());
    // Selected: its handles paint on the top canvas.
    assert!(arcs_of(&w.drawings_canvas(), 4.5) >= 2);

    // Click empty space: the selection ends.
    w.ctl.deselect_now();
    assert!(w.ctl.selected_ids().is_empty());

    // A click on the line selects it again and paints its handles.
    w.host.clear_canvases();
    let mid = w.px(90, 115.0);
    assert!(w.ctl.claim(mid.0, mid.1));
    w.click(mid);
    w.idle();
    assert_eq!(w.ctl.selected_ids(), vec![line.id.clone()]);
    assert!(arcs_of(&w.drawings_canvas(), 4.5) >= 2, "handles painted");

    // A drag moves it: ONE edit, so one undo step.
    let before = w.drawings()[0].anchors.clone();
    w.ctl.pointer_move(mid.0, mid.1, OFF, false, false);
    w.ctl.pointer_down(mid.0, mid.1, OFF, false, false);
    w.ctl
        .pointer_move(mid.0 + 10.0, mid.1 + 8.0, OFF, false, false);
    w.ctl
        .pointer_move(mid.0 + 40.0, mid.1 + 20.0, OFF, false, false);
    w.ctl.pointer_up(mid.0 + 40.0, mid.1 + 20.0, OFF);
    let after = w.drawings()[0].anchors.clone();
    assert_ne!(before, after);
    assert!(after[0].time > before[0].time);
    assert!(after[0].price < before[0].price); // moved down
    assert!(
        (after[1].time - after[0].time - (before[1].time - before[0].time)).abs() < 1.0,
        "a body drag keeps the shape"
    );
    w.core.undo();
    assert_eq!(
        w.drawings()[0].anchors,
        before,
        "one history entry per gesture"
    );
    w.core.redo();
    assert_eq!(w.drawings()[0].anchors, after);

    // Delete removes it (it is selected) and undo restores it.
    assert_eq!(w.ctl.selected_ids().len(), 1);
    assert!(w.ctl.handle_key(&key("Delete"), false));
    assert!(w.drawings().is_empty());
    assert!(w.ctl.drawing_docs().is_empty());
    assert!(w.ctl.handle_key(&ctrl("z"), false));
    assert_eq!(w.drawings().len(), 1);
    assert_eq!(w.ctl.drawing_docs().len(), 1);
    // The undo pruned the (gone) drawing from the selection: Delete has no target.
    assert!(w.ctl.selected_ids().is_empty());
    assert!(!w.ctl.handle_key(&key("Delete"), false));
}

#[test]
fn undo_and_redo_chords_stand_down_when_the_host_owns_history() {
    let w = world();
    w.place_trendline();
    w.r.apply_feature("historyChords", &json!(false));
    assert!(!w.ctl.handle_key(&ctrl("z"), false));
    assert_eq!(w.drawings().len(), 1);
    w.r.apply_feature("historyChords", &json!(true));
    assert!(w.ctl.handle_key(&ctrl("z"), false));
    assert!(w.drawings().is_empty());
}

#[test]
fn escape_cancels_a_placement_and_the_tool_finishes() {
    let w = world();
    w.core.set_tool(Some("trendline"));
    let a = w.px(60, 110.0);
    w.click(a);
    assert!(w.ctl.handle_key(&key("Escape"), false));
    assert!(w.drawings().is_empty());
    assert_eq!(w.core.get_tool(), None);
}

#[test]
fn a_right_click_disarms_an_armed_idle_tool_and_cancels_a_placement() {
    let w = world();
    assert!(!w.ctl.cancel_placement()); // nothing to cancel: the context menu may open
    w.core.set_tool(Some("trendline"));
    assert!(w.ctl.cancel_placement()); // armed, no anchor: plain disarm
    assert_eq!(w.core.get_tool(), None);
    w.core.set_tool(Some("trendline"));
    w.click(w.px(60, 110.0)); // one anchor placed
    assert!(w.ctl.cancel_placement());
    assert_eq!(w.core.get_tool(), None);
    assert!(w.drawings().is_empty());
}

#[test]
fn a_right_click_in_stay_mode_still_reverts_to_the_pointer() {
    let w = world();
    w.core.set_stay_mode(true);
    w.core.set_tool(Some("trendline"));
    w.click(w.px(60, 110.0));
    assert!(w.ctl.cancel_placement());
    assert_eq!(
        w.core.get_tool(),
        None,
        "stay mode kept it armed, the right-click disarms"
    );
}

// ── freehand, text, other panes ──

#[test]
fn a_freehand_stroke_is_captured_while_dragging_and_the_brush_stays_armed() {
    let w = world();
    w.core.set_tool(Some("freehand"));
    let start = w.px(40, 105.0);
    w.ctl.pointer_move(start.0, start.1, OFF, false, false);
    w.ctl.pointer_down(start.0, start.1, OFF, false, false);
    for i in 1..=30 {
        w.ctl.pointer_move(
            start.0 + i as f64 * 6.0,
            start.1 + (i as f64 * 0.4).sin() * 20.0,
            OFF,
            false,
            false,
        );
    }
    // While drawing, the ghost paints on the drawings canvas.
    assert!(!w.drawings_canvas().calls("stroke").is_empty());
    w.ctl.pointer_up(start.0 + 180.0, start.1, OFF);
    w.idle();
    let docs = w.drawings();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].type_key, "freehand");
    assert!(docs[0].anchors.len() > 10);
    assert_eq!(w.core.get_tool().as_deref(), Some("freehand")); // the brush stays armed
}

#[test]
fn a_brush_click_without_a_drag_keeps_nothing() {
    let w = world();
    w.core.set_tool(Some("freehand"));
    w.click(w.px(40, 105.0));
    assert!(w.drawings().is_empty());
}

#[test]
fn a_text_label_opens_its_inline_editor_and_commits_through_the_seam() {
    let w = world();
    w.core.set_tool(Some("text"));
    w.click(w.px(80, 112.0));
    w.idle();
    let docs = w.drawings();
    assert_eq!(docs.len(), 1);
    let id = docs[0].id.clone();
    assert_eq!(docs[0].type_key, "text");
    // The ink is fixed at creation: contrast against the dark plot background.
    assert_eq!(
        docs[0].text.as_ref().unwrap().color.as_deref(),
        Some("#ffffff")
    );

    let events = editor_events(&w.ui());
    let Some(TextEditorEvent::Open(spec)) = events
        .iter()
        .find(|e| matches!(e, TextEditorEvent::Open(_)))
    else {
        panic!("the inline editor opened: {events:?}");
    };
    assert_eq!(spec.id, id);
    assert_eq!(spec.kind, TextEditorKind::Label);
    assert_eq!(spec.placeholder, "Enter Text");
    assert_eq!(spec.background, "transparent");
    assert!(spec.width > 8.0 && spec.height > 0.0);
    assert_eq!(w.ctl.text_editing().as_deref(), Some(id.as_str()));

    // Typing flows through the seam; Ctrl/Cmd+Enter commits it.
    w.ctl.text_editor_input("Hello");
    w.ctl.text_editor_commit();
    w.idle();
    assert_eq!(w.ctl.text_editing(), None);
    assert_eq!(w.drawings()[0].text.as_ref().unwrap().value, "Hello");
    assert!(editor_events(&w.ui()).contains(&TextEditorEvent::Close));
}

#[test]
fn a_text_label_left_empty_is_dropped_and_escape_restores_the_old_text() {
    let w = world();
    w.core.set_tool(Some("text"));
    w.click(w.px(80, 112.0));
    w.idle();
    w.ctl.text_editor_commit(); // nothing typed
    assert!(
        w.drawings().is_empty(),
        "an empty label would paint nothing"
    );

    w.core.set_tool(Some("text"));
    w.click(w.px(80, 112.0));
    w.idle();
    w.ctl.text_editor_input("Keep");
    w.ctl.text_editor_blur(false); // clicking away keeps the text
    assert_eq!(w.drawings()[0].text.as_ref().unwrap().value, "Keep");
    let id = w.drawings()[0].id.clone();
    // Double-click edits it in place; Escape puts back what was there.
    let at = w.drawings()[0].anchors[0];
    let p =
        w.r.with_drawing_projector(|p| (p.x_of(at.time), p.y_of(at.price, "price").unwrap()));
    assert!(w.ctl.dbl_click(p.0 + 8.0, p.1 + 8.0));
    assert_eq!(w.ctl.text_editing().as_deref(), Some(id.as_str()));
    w.ctl.text_editor_input("Changed");
    w.ctl.text_editor_cancel();
    assert_eq!(w.drawings()[0].text.as_ref().unwrap().value, "Keep");
}

#[test]
fn a_callout_editor_fills_its_bubble() {
    let w = world();
    w.core.set_tool(Some("callout"));
    let a = w.px(60, 110.0);
    let b = w.px(90, 125.0);
    w.click(a);
    w.ctl.pointer_move(b.0, b.1, OFF, false, false);
    w.click(b);
    w.idle();
    assert_eq!(w.drawings().len(), 1);
    let events = editor_events(&w.ui());
    let Some(TextEditorEvent::Open(spec)) = events
        .iter()
        .find(|e| matches!(e, TextEditorEvent::Open(_)))
    else {
        panic!("the callout's editor opened");
    };
    assert_eq!(spec.kind, TextEditorKind::Callout);
    assert_eq!(spec.padding, (5.0, 8.0));
    assert_eq!(spec.border_radius, 5.0);
    assert!(spec.background.starts_with('#'));
    assert_eq!(spec.text, "Callout");
}

#[test]
fn a_tool_works_on_a_non_price_pane() {
    let w = world();
    w.r.ensure_pane(&Pane {
        id: "rsi".into(),
        kind: PaneKind::Study,
        order: 1,
        height_weight: None,
        title: None,
    });
    let times: Vec<i64> = w.bars.iter().map(|b| b.time).collect();
    let osc: Vec<Option<f64>> = (0..200)
        .map(|i| Some(50.0 + (i as f64 * 0.2).sin() * 20.0))
        .collect();
    w.r.mount_indicator(&line_model("rsi", "rsi", false, &times, &osc));
    w.idle();
    let (top, height, _) = w.r.pane_bounds_for("rsi");
    assert!(height > 20.0);
    let y = top + height / 2.0;
    let x1 = w.px(60, 100.0).0;
    let x2 = w.px(120, 100.0).0;
    w.core.set_tool(Some("trendline"));
    w.click((x1, y));
    w.ctl.pointer_move(x2, y - 10.0, OFF, false, false);
    w.click((x2, y - 10.0));
    w.idle();
    let docs = w.drawings();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].pane_id, "rsi");
    // Over the study pane's series stack: the body paints on the top canvas, clipped to
    // its pane.
    assert!(!w.drawings_canvas().calls("stroke").is_empty());
    assert!(!w.drawings_canvas().calls("clip").is_empty());
    // And it is hit-testable there.
    w.ctl.deselect_now();
    assert!(w.ctl.claim((x1 + x2) / 2.0, y - 5.0));
}

// ── modes: measure and eraser ──

#[test]
fn shift_press_starts_the_ruler_and_the_second_click_finishes_it() {
    let w = world();
    let a = w.px(60, 110.0);
    let b = w.px(120, 120.0);
    assert!(!w.ctl.claim(a.0, a.1));
    assert!(w.ctl.begin_measure_at(a.0, a.1, OFF));
    assert!(w.ctl.is_measure_mode());
    w.ctl.pointer_up(a.0, a.1, OFF); // a click: wait for the second
    w.ctl.pointer_move(b.0, b.1, OFF, false, false);
    assert!(w.ctl.claim(0.0, 0.0)); // the ruler captures its clicks
    w.host.clear_canvases();
    w.ctl.pointer_down(b.0, b.1, OFF, false, false);
    assert!(
        !w.ctl.is_measure_mode(),
        "the second click disarms, keeping the ruler shown"
    );
    assert!(!w.drawings_canvas().calls("fillRect").is_empty());
    // The next press clears the finished ruler.
    w.ctl.clear_transient();
    w.host.clear_canvases();
    w.ctl.pointer_move(a.0, a.1, OFF, false, false);
    w.ctl.pointer_up(a.0, a.1, OFF);
}

#[test]
fn measure_and_eraser_are_mutually_exclusive_and_report_through_the_core() {
    let w = world();
    w.ctl.toolbar_toggle_measure();
    assert!(w.ctl.is_measure_mode());
    assert_eq!(
        w.core.get_mode(),
        Some(crate::ports::chart_renderer::DrawingMode::Measure)
    );
    w.ctl.toolbar_toggle_eraser();
    assert!(!w.ctl.is_measure_mode());
    assert!(w.ctl.is_eraser_mode());
    assert_eq!(
        w.core.get_mode(),
        Some(crate::ports::chart_renderer::DrawingMode::Eraser)
    );
    // Arming a real tool exits the eraser.
    w.core.set_tool(Some("trendline"));
    assert!(!w.ctl.is_eraser_mode());
    assert_eq!(w.core.get_mode(), None);
    // The core can drive the modes too.
    w.core
        .set_mode(Some(crate::ports::chart_renderer::DrawingMode::Eraser));
    assert!(w.ctl.is_eraser_mode());
    w.core.set_mode(None);
    assert!(!w.ctl.is_eraser_mode());
}

#[test]
fn the_eraser_deletes_on_click_and_while_dragging_across_drawings() {
    let w = world();
    let first = w.place_trendline();
    w.core.set_tool(Some("hline"));
    w.click(w.px(100, 95.0));
    w.idle();
    assert_eq!(w.drawings().len(), 2);
    w.ctl.toolbar_toggle_eraser();
    assert_eq!(w.ctl.cursor_at(0.0, 0.0), Some("pointer"));
    let mid = w.px(90, 115.0);
    let on_first = w.drawings().iter().any(|d| d.id == first.id);
    assert!(on_first);
    w.ctl.pointer_down(mid.0, mid.1, OFF, false, false);
    assert_eq!(w.drawings().len(), 1);
    // Hovering without the button erases nothing; holding does.
    w.ctl.pointer_up(mid.0, mid.1, OFF);
    let line = w.px(100, 95.0);
    w.ctl.pointer_move(line.0, line.1, OFF, false, false);
    assert_eq!(w.drawings().len(), 1);
    w.ctl.pointer_down(0.0, 0.0, OFF, false, false);
    w.ctl.pointer_move(line.0, line.1, OFF, false, false);
    assert!(w.drawings().is_empty());
}

#[test]
fn middle_click_delete_takes_a_whole_selection_and_spares_locked_members() {
    let w = world();
    w.core.set_tool(Some("hline"));
    w.click(w.px(100, 95.0));
    w.idle();
    w.core.set_tool(Some("hline"));
    w.click(w.px(100, 130.0));
    w.idle();
    let ids: Vec<String> = w.drawings().iter().map(|d| d.id.clone()).collect();
    assert_eq!(ids.len(), 2);
    w.core.select(&ids, false);
    let p = w.px(100, 95.0);
    assert!(w.ctl.delete_at(p.0, p.1, true));
    assert!(w.drawings().is_empty());
}

// ── selection, popup, keyboard ──

#[test]
fn a_marquee_selects_what_it_touches_and_a_multi_selection_gets_one_bar() {
    let w = world();
    w.core.set_tool(Some("hline"));
    w.click(w.px(100, 95.0));
    w.idle();
    w.core.set_tool(Some("hline"));
    w.click(w.px(100, 112.0));
    w.idle();
    w.ctl.deselect_now();
    w.host.clear_drawings_ui_events();
    assert!(w.ctl.begin_marquee_at(2.0, 2.0));
    assert!(w.ctl.claim(2.0, 2.0), "the sweep is in flight");
    w.ctl.pointer_move(700.0, 400.0, OFF, false, true);
    w.host.clear_canvases();
    w.ctl.pointer_move(710.0, 405.0, OFF, false, true);
    assert!(
        !w.drawings_canvas().calls("strokeRect").is_empty(),
        "the box paints"
    );
    w.ctl.pointer_up(710.0, 405.0, OFF);
    assert_eq!(w.ctl.selected_ids().len(), 2);
    let opens = popup_opens(&w.ui());
    let last = opens.last().expect("the multi-selection bar opened");
    assert!(last.multi);
    assert_eq!(last.ids.len(), 2);
    // One bar edits both: a patch becomes ONE edit-many (one undo step).
    let mut patch = Props::new();
    patch.insert("style.lineColor".into(), json!("#ff0000"));
    w.ctl.popup_patch(&patch);
    assert!(w.drawings().iter().all(|d| d.style.line_color == "#ff0000"));
    w.core.undo();
    assert!(w.drawings().iter().all(|d| d.style.line_color != "#ff0000"));
}

#[test]
fn the_popup_edits_one_drawing_and_reports_its_dismissal() {
    let w = world();
    let line = w.place_trendline();
    let mut patch = Props::new();
    patch.insert("style.lineWidth".into(), json!(5));
    w.ctl.popup_patch(&patch);
    assert_eq!(w.drawings()[0].style.line_width, 5.0);
    w.ctl.popup_set_locked(true);
    assert!(w.drawings()[0].locked);
    // A locked drawing never drags: a press-move-release is a click.
    let mid = w.px(90, 115.0);
    let before = w.drawings()[0].anchors.clone();
    w.ctl.pointer_down(mid.0, mid.1, OFF, false, false);
    w.ctl
        .pointer_move(mid.0 + 30.0, mid.1 + 30.0, OFF, false, false);
    w.ctl.pointer_up(mid.0 + 30.0, mid.1 + 30.0, OFF);
    assert_eq!(w.drawings()[0].anchors, before);
    w.ctl.popup_set_locked(false);
    // The popup closing itself on an outside press clears the selection...
    assert!(w.ctl.is_popup_open());
    w.ctl.popup_dismissed(false);
    assert!(!w.ctl.is_popup_open());
    assert!(w.ctl.selected_ids().is_empty());
    // ...unless the press carried a multi-select modifier.
    w.ctl.popup_dismissed(true);
    // Remove deletes it and closes the popup.
    w.core.open_settings(&line.id);
    assert!(w.ctl.is_popup_open());
    w.ctl.popup_remove();
    assert!(w.drawings().is_empty());
    assert!(!w.ctl.is_popup_open());
}

#[test]
fn duplicate_and_reorder_go_through_the_popup() {
    let w = world();
    let line = w.place_trendline();
    w.ctl.popup_duplicate();
    assert_eq!(w.drawings().len(), 2);
    assert!(!w.ctl.is_popup_open());
    // The copy becomes the selection.
    assert_eq!(w.ctl.selected_ids().len(), 1);
    assert_ne!(w.ctl.selected_ids()[0], line.id);
    w.core.open_settings(&line.id);
    let before_z = w
        .drawings()
        .iter()
        .find(|d| d.id == line.id)
        .unwrap()
        .z_index;
    w.ctl.popup_reorder(ReorderTo::Back);
    let after_z = w
        .drawings()
        .iter()
        .find(|d| d.id == line.id)
        .unwrap()
        .z_index;
    assert!(after_z < before_z, "sent behind everything");
}

#[test]
fn reset_restores_the_factory_look_and_a_preview_leaves_no_trace() {
    let w = world();
    w.place_trendline();
    let factory = w.drawings()[0].style.clone();
    let mut patch = Props::new();
    patch.insert("style.lineColor".into(), json!("#00ff00"));
    w.ctl.popup_patch(&patch);
    assert_eq!(w.drawings()[0].style.line_color, "#00ff00");
    w.ctl.popup_reset_settings();
    assert_eq!(w.drawings()[0].style, factory);
    assert!(
        w.ctl.is_popup_open(),
        "the bar rebuilds over the restored defaults"
    );
    // A hovered style paints on the drawing without an edit; leaving puts the look back.
    let style = DrawingToolDefaults {
        style: Some(serde_json::from_value(json!({ "lineColor": "#123456" })).unwrap()),
        ..DrawingToolDefaults::default()
    };
    w.ctl.popup_preview(Some(&style));
    assert_eq!(
        w.ctl
            .drawing_doc(&w.drawings()[0].id)
            .unwrap()
            .style
            .line_color,
        "#123456"
    );
    assert_eq!(
        w.drawings()[0].style,
        factory,
        "the core never saw the preview"
    );
    w.ctl.popup_preview(None);
    assert_eq!(
        w.ctl.drawing_doc(&w.drawings()[0].id).unwrap().style,
        factory
    );
}

#[test]
fn arrow_keys_nudge_the_selection_as_one_edit_and_copy_paste_duplicates() {
    let w = world();
    w.place_trendline();
    let before = w.drawings()[0].anchors.clone();
    assert!(w.ctl.handle_key(&key("ArrowRight"), false));
    let moved = w.drawings()[0].anchors.clone();
    assert!(moved[0].time > before[0].time);
    assert_eq!(moved[0].price, before[0].price);
    w.core.undo();
    assert_eq!(w.drawings()[0].anchors, before, "a nudge is one undo step");
    assert!(w.ctl.handle_key(&ctrl("c"), false));
    assert!(w.ctl.handle_key(&ctrl("v"), false));
    assert_eq!(w.drawings().len(), 2);
    // Typing into a field leaves the keys alone.
    assert!(!w.ctl.handle_key(&key("ArrowLeft"), true));
}

#[test]
fn ctrl_drag_copies_and_commits_one_clone() {
    let w = world();
    w.place_trendline();
    w.ctl.deselect_now();
    let mid = w.px(90, 115.0);
    w.ctl.pointer_move(mid.0, mid.1, OFF, false, false);
    w.ctl
        .pointer_down(mid.0, mid.1, SnapMode::Strong, false, true);
    w.ctl
        .pointer_move(mid.0 + 40.0, mid.1 + 30.0, SnapMode::Strong, false, true);
    assert_eq!(w.drawings().len(), 1, "the copy is transient until release");
    w.ctl
        .pointer_up(mid.0 + 40.0, mid.1 + 30.0, SnapMode::Strong);
    assert_eq!(w.drawings().len(), 2);
    assert!(w.ctl.selected_ids().len() == 1);
    w.core.undo();
    assert_eq!(w.drawings().len(), 1, "the clone is one undo step");
}

#[test]
fn double_click_opens_the_panel_and_finishes_a_polyline() {
    let w = world();
    let line = w.place_trendline();
    w.ctl.deselect_now();
    w.host.clear_drawings_ui_events();
    let mid = w.px(90, 115.0);
    assert!(w.ctl.dbl_click(mid.0, mid.1));
    assert!(
        w.ui()
            .contains(&DrawingsUiEvent::Popup(PopupEvent::OpenPanel))
    );
    assert_eq!(w.ctl.selected_ids(), vec![line.id]);
    assert!(!w.ctl.dbl_click(2.0, 2.0)); // empty space: the chart's own double click

    w.core.set_tool(Some("polyline"));
    for (i, p) in [(30, 100.0), (50, 125.0), (70, 105.0)] {
        w.click(w.px(i, p));
    }
    w.click(w.px(70, 105.0)); // the second press of the double-click: a duplicate point
    assert!(w.ctl.dbl_click(w.px(70, 105.0).0, w.px(70, 105.0).1));
    w.idle();
    let poly = w
        .drawings()
        .into_iter()
        .find(|d| d.type_key == "polyline")
        .unwrap();
    assert_eq!(poly.anchors.len(), 3);
}

#[test]
fn enter_finishes_a_polyline_and_the_hint_shows_while_armed() {
    let w = world();
    w.core.set_tool(Some("polyline"));
    w.click(w.px(30, 100.0));
    w.click(w.px(50, 125.0));
    w.click(w.px(70, 105.0));
    assert!(w.ctl.handle_key(&key("Enter"), false));
    w.idle();
    assert_eq!(w.drawings().len(), 1);
    // The magnifier tool's placement hint paints while it is armed and idle.
    w.core.set_tool(Some("magnifier"));
    w.host.clear_canvases();
    w.r.repaint_user_drawings();
    assert!(
        !w.drawings_canvas().texts().is_empty(),
        "the placement hint pill"
    );
}

#[test]
fn a_drag_placed_tool_finishes_on_release() {
    let w = world();
    w.core.set_tool(Some("magnifier"));
    let a = w.px(60, 110.0);
    let b = w.px(100, 125.0);
    w.ctl.pointer_move(a.0, a.1, OFF, false, false);
    w.ctl.pointer_down(a.0, a.1, OFF, false, false);
    w.ctl.pointer_move(b.0, b.1, OFF, false, false);
    assert!(w.drawings().is_empty());
    w.ctl.pointer_up(b.0, b.1, OFF);
    w.idle();
    assert_eq!(w.drawings().len(), 1);
    assert_eq!(w.drawings()[0].type_key, "magnifier");
}

#[test]
fn the_magnifier_chip_opens_its_menu_instead_of_selecting() {
    let w = world();
    w.core.set_tool(Some("magnifier"));
    let a = w.px(60, 110.0);
    let b = w.px(110, 140.0);
    w.ctl.pointer_move(a.0, a.1, OFF, false, false);
    w.ctl.pointer_down(a.0, a.1, OFF, false, false);
    w.ctl.pointer_move(b.0, b.1, OFF, false, false);
    w.ctl.pointer_up(b.0, b.1, OFF);
    w.idle();
    w.ctl.deselect_now();
    let id = w.drawings()[0].id.clone();
    let rect = w
        .ctl
        .st
        .borrow()
        .painter
        .chip_rect(&id)
        .expect("the chip painted");
    let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    assert!(w.ctl.claim(cx, cy));
    assert_eq!(w.ctl.cursor_at(cx, cy), Some("pointer"));
    w.host.clear_drawings_ui_events();
    w.ctl.pointer_down(cx, cy, OFF, false, false);
    let menu = w.ui().into_iter().find_map(|e| match e {
        DrawingsUiEvent::MagnifierMenu(m) => Some(m),
        _ => None,
    });
    let menu = menu.expect("the menu opened");
    assert_eq!(menu.id, id);
    assert_eq!(menu.current, "auto");
    assert!(
        w.ctl.selected_ids().is_empty(),
        "the chip swallowed the press"
    );
    w.ctl.magnifier_timeframe_picked(&id, "5");
    let doc = w.drawings().pop().unwrap();
    assert_eq!(doc.props.unwrap()["timeframe"], json!("5"));
}

// ── timeframe visibility and hover ──

#[test]
fn a_drawing_limited_to_other_timeframes_is_neither_painted_nor_clickable() {
    let w = world();
    let line = w.place_trendline();
    w.ctl.deselect_now();
    let mid = w.px(90, 115.0);
    assert!(w.ctl.claim(mid.0, mid.1));
    // The sample bars are 1-minute ones: a drawing limited to the daily band hides.
    w.core.update(
        &line.id,
        crate::drawings::controller::DrawingUpdate {
            show_on: Some(Some(vec!["D".to_string()])),
            ..Default::default()
        },
    );
    w.idle();
    assert!(!w.ctl.claim(mid.0, mid.1));
    assert_eq!(w.ctl.cursor_at(mid.0, mid.1), None);
    assert_eq!(w.drawings().len(), 1, "still in the model");
    assert_eq!(w.ctl.drawing_docs().len(), 1);
}

#[test]
fn hovering_shows_handles_and_turning_the_layer_off_drops_them() {
    let w = world();
    w.place_trendline();
    w.ctl.deselect_now();
    let mid = w.px(90, 115.0);
    w.ctl.pointer_move(mid.0, mid.1, OFF, false, false);
    assert!(w.ctl.hovered_id().is_some());
    assert_eq!(w.ctl.cursor_at(mid.0, mid.1), Some("pointer"));
    w.r.apply_feature("drawingsInteractive", &json!(false));
    assert_eq!(w.ctl.hovered_id(), None);
    w.idle();
}

// ── ownership rules ──

#[test]
fn a_resize_with_the_editor_open_commits_on_a_frame_not_under_the_renderers_borrow() {
    let w = world();
    w.core.set_tool(Some("text"));
    w.click(w.px(80, 112.0));
    w.idle();
    w.ctl.text_editor_input("Typed");
    w.host.set_size(780.0, 400.0);
    w.r.resize(); // the hook runs under the renderer's borrow: nothing may re-enter it
    w.idle();
    assert_eq!(w.ctl.text_editing(), None);
    assert_eq!(w.drawings()[0].text.as_ref().unwrap().value, "Typed");
}

#[test]
fn replacing_the_default_layer_destroys_it_and_unregisters_the_default_accessor() {
    let w = world();
    w.place_trendline();
    struct Hook;
    impl UserDrawingsHook for Hook {
        fn prepare_slices(
            &self,
            _: &[String],
            _: &FrameView,
        ) -> std::collections::HashMap<String, Vec<DrawingSlice>> {
            Default::default()
        }
        fn render(&self, _: &FrameView) {}
    }
    struct Port;
    impl DrawingsRendererPort for Port {
        fn set_toolbar(&self, _: &serde_json::Value) {}
        fn show_toolbar(&self, _: bool) {}
        fn sync_drawings(&self, _: &[SerializedDrawing]) {}
        fn set_active_tool(
            &self,
            _: Option<&str>,
            _: Option<&DrawingStyle>,
            _: Option<&DrawingToolDefaults>,
        ) {
        }
        fn set_selection(&self, _: &[String]) {}
        fn open_settings(&self, _: &str) {}
        fn on_drawing_intent(&self, _: Listener<DrawingIntent>) -> Unsubscribe {
            crate::runtime::noop_unsubscribe()
        }
    }
    w.host.clear_drawings_ui_events();
    w.r.set_user_drawings(Some((Rc::new(Hook), Rc::new(Port))));
    assert!(w.r.user_drawings_controller().is_none());
    assert!(w.ui().contains(&DrawingsUiEvent::Popup(PopupEvent::Close)));
    assert!(
        w.ctl.drawing_docs().is_empty(),
        "the old layer let go of its projection"
    );
}

#[test]
fn stack_range_reports_the_series_stack_in_z_terms() {
    let w = world();
    let range = w.ctl.stack_range("price").expect("a range");
    assert_eq!(range.price, Some(0.0));
    assert!(range.front >= 0.0 && range.back <= 0.0);
    // A drawing the core places on the price pane starts just under the candles.
    let line = w.place_trendline();
    assert_eq!(line.z_index, -0.5);
}

// ── placement progress, the remote ghost, the editor following the chart ──

#[test]
fn placement_progress_reaches_the_core_as_draft_intents_one_frame_later() {
    use crate::events::types::{ChartEvent, ChartEventName};
    let w = world();
    let drafts: Rc<std::cell::RefCell<Vec<bool>>> = Rc::default();
    {
        let drafts = Rc::clone(&drafts);
        w.events.on(ChartEventName::DrawingDraft, move |e| {
            if let ChartEvent::DrawingDraft { doc } = e {
                drafts.borrow_mut().push(doc.is_some());
            }
        });
    }
    w.core.set_tool(Some("trendline"));
    w.click(w.px(60, 110.0));
    let b = w.px(90, 118.0);
    w.ctl.pointer_move(b.0, b.1, OFF, false, false); // the ghost appears
    // The renderer's frame called the controller under its borrow: the intent waits.
    w.idle();
    assert_eq!(drafts.borrow().last(), Some(&true));
    w.click(b); // placed: the ghost ends
    w.idle();
    assert_eq!(drafts.borrow().last(), Some(&false));
}

#[test]
fn a_remote_placement_paints_as_a_ghost_and_clears() {
    let w = world();
    let doc = serde_json::from_value::<SerializedDrawing>(json!({
        "id": "remote", "type": "trendline", "paneId": "price",
        "anchors": [
            { "time": w.bars[60].time, "price": 110.0 },
            { "time": w.bars[90].time, "price": 118.0 },
        ],
        "style": {},
    }))
    .unwrap();
    w.host.clear_canvases();
    w.ctl.set_external_ghost(Some(&doc));
    let c = w.drawings_canvas();
    assert!(!c.calls("stroke").is_empty());
    assert!(
        c.calls("globalAlpha")
            .iter()
            .any(|a| a.get(1).and_then(|v| v.as_f64()) == Some(0.7))
    );
    w.host.clear_canvases();
    w.ctl.set_external_ghost(None);
    assert!(w.drawings_canvas().calls("stroke").is_empty());
}

#[test]
fn the_open_editor_follows_the_chart_when_it_pans() {
    let w = world();
    w.core.set_tool(Some("text"));
    w.click(w.px(80, 112.0));
    w.idle();
    w.host.clear_drawings_ui_events();
    w.r.pan_by_pixels(40.0);
    w.idle();
    let layouts: Vec<TextEditorSpec> = editor_events(&w.ui())
        .into_iter()
        .filter_map(|e| match e {
            TextEditorEvent::Layout(spec) => Some(spec),
            _ => None,
        })
        .collect();
    assert!(
        !layouts.is_empty(),
        "the host is told where the anchor went"
    );
    assert!(
        layouts
            .iter()
            .all(|spec| spec.kind == TextEditorKind::Label)
    );
}

#[test]
fn the_destroyed_layer_reports_its_close_events_and_goes_quiet() {
    let w = world();
    w.place_trendline();
    w.host.clear_drawings_ui_events();
    w.r.destroy();
    assert!(w.ui().contains(&DrawingsUiEvent::Popup(PopupEvent::Close)));
    // Entry points after destroy do nothing harmful.
    assert!(!w.ctl.claim(1.0, 1.0));
    w.ctl.pointer_move(1.0, 1.0, OFF, false, false);
}
