//! The stateful workspace tests, on the in-memory host with a fake provider: boot, layout
//! changes, sync links, persistence round-trips. Ports of the behaviors
//! `workspace-core`, `workspace-persist`, `workspace-destroy-flush`,
//! `workspace-drawings-sync`, `workspace-style-sync` and `workspace-data-unavailable` pin in
//! the original (whose shell is verified in a browser there), plus the end-to-end cases the
//! brief asks for.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};

use super::*;
use crate::state::{SyncKind, SyncSetting};
use crate::workspace::host::ToastKind;
use crate::workspace::persist::{
    MemoryStorage, StorageRead, WorkspaceStorage, memory_storage_adapter,
};
use crate::workspace::rig::{FakeProvider, Rig, T0};
use crate::workspace::splitters::Axis;

const QUAD: [(&str, &str); 4] = [("a", "AAA"), ("b", "BBB"), ("c", "CCC"), ("d", "DDD")];

fn quad() -> Rig {
    Rig::new("4", &QUAD)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn chart_symbol(ws: &Workspace, id: &str) -> Option<String> {
    ws.cell(id)?.try_chart()?.market().symbol
}

fn counter(ws: &Workspace, name: WorkspaceEventName) -> Rc<Cell<usize>> {
    let n = Rc::new(Cell::new(0));
    let c = n.clone();
    ws.on(name, move |_| c.set(c.get() + 1));
    n
}

// ── boot ──────────────────────────────────────────────────────

#[test]
fn a_2x2_workspace_boots_four_cells_each_with_its_own_symbol() {
    let rig = quad();
    assert_eq!(rig.ids(), ["a", "b", "c", "d"]);
    assert_eq!(rig.symbols(), ["AAA", "BBB", "CCC", "DDD"]);
    for (id, symbol) in QUAD {
        assert_eq!(chart_symbol(&rig.ws, id).as_deref(), Some(symbol));
    }
    assert_eq!(rig.host.created(), ["a", "b", "c", "d"]);
    assert_eq!(rig.host.mounted(), ["a", "b", "c", "d"]);
    assert_eq!(rig.ws.active_id().as_deref(), Some("a"));
    assert_eq!(rig.ws.layout().id, "4");
    // Every cell's chart actually loaded its own bars through the shared feed.
    let asked: Vec<String> = rig
        .provider
        .calls
        .borrow()
        .iter()
        .map(|(t, _)| t.clone())
        .collect();
    for (_, symbol) in QUAD {
        assert!(
            asked.iter().any(|t| t == symbol),
            "{symbol} was never fetched"
        );
    }
}

#[test]
fn the_first_grid_presentation_describes_the_layout_and_the_active_cell() {
    let rig = quad();
    let grid = rig.host.last_grid().expect("a presentation");
    assert_eq!(grid.layout, "4");
    assert_eq!(
        (grid.cols.clone(), grid.rows.clone()),
        (vec![1.0, 1.0], vec![1.0, 1.0])
    );
    assert!(grid.multi && grid.maximized.is_none());
    let ids: Vec<&str> = grid.cells.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
    assert_eq!(grid.cells.iter().filter(|c| c.active).count(), 1);
    assert!(grid.cells[0].active);
    assert!(
        grid.cells
            .iter()
            .all(|c| !c.hidden && c.grid_area.is_none())
    );
    let slots: Vec<&str> = grid.cells.iter().map(|c| c.slot_id.as_str()).collect();
    assert_eq!(slots, ["c1", "c2", "c3", "c4"]);
}

#[test]
fn undeclared_slots_get_auto_identities_and_extra_declarations_wait_in_the_pool() {
    // Two declared of four slots: the rest mint c1, c2.
    let rig = Rig::new("4", &[("btc", "AAA"), ("eth", "BBB")]);
    assert_eq!(rig.ids(), ["btc", "eth", "c1", "c2"]);
    // Three declared on a two-slot grid: the third waits.
    let rig = Rig::new("2h", &[("x", "AAA"), ("y", "BBB"), ("z", "CCC")]);
    assert_eq!(rig.ids(), ["x", "y"]);
    let state = rig.ws.get_state();
    let saved: Vec<&str> = state.charts.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        saved,
        ["x", "y"],
        "never-built declarations have no state to save"
    );
    rig.ws.set_layout("4").unwrap();
    rig.settle();
    assert_eq!(
        rig.ids(),
        ["x", "y", "z", "c1"],
        "the waiting declaration takes the next slot"
    );
    assert_eq!(
        rig.ws.cell("z").unwrap().symbol(),
        "CCC",
        "booted from its own seed"
    );
}

#[test]
fn an_unknown_layout_id_is_an_error_and_keeps_the_grid() {
    let rig = quad();
    let err = rig.ws.set_layout("nope").unwrap_err();
    assert_eq!(err, WorkspaceError::UnknownLayout("nope".into()));
    assert_eq!(rig.ws.layout().id, "4");
    let platform = crate::runtime::testing::ManualPlatform::new(0.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "4", &QUAD);
    opts.layout = LayoutChoice::Id("missing".into());
    let env = crate::vela::VelaEnv::new(platform.handle());
    let host = crate::workspace::testing::TestWorkspaceHost::new(400.0, 300.0);
    assert!(Workspace::new(host, env, opts).is_err());
}

// ── active cell ───────────────────────────────────────────────

#[test]
fn activating_a_cell_switches_the_active_flag_and_reports_it() {
    let rig = quad();
    type Seen = Vec<(String, Option<String>)>;
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let s = seen.clone();
    rig.ws.on(WorkspaceEventName::CellActive, move |e| {
        if let WorkspaceEvent::CellActive { id, prev } = e {
            s.borrow_mut().push((id.clone(), prev.clone()));
        }
    });
    rig.ws.activate("c");
    assert_eq!(rig.ws.active_id().as_deref(), Some("c"));
    assert_eq!(
        seen.borrow().as_slice(),
        [("c".to_string(), Some("a".to_string()))]
    );
    rig.ws.activate("c"); // already active: silent
    rig.ws.activate("nope"); // not a live cell
    assert_eq!(seen.borrow().len(), 1);
    let grid = rig.host.last_grid().unwrap();
    let active: Vec<&str> = grid
        .cells
        .iter()
        .filter(|c| c.active)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(active, ["c"]);
}

#[test]
fn the_projection_follows_the_active_cell_and_fires_only_for_it() {
    let rig = quad();
    let p = rig.ws.projection().expect("a projection");
    assert_eq!(
        (p.cell_id.as_str(), p.symbol.as_str(), p.timeframe.as_str()),
        ("a", "AAA", "60")
    );
    let fired = Rc::new(RefCell::new(Vec::<String>::new()));
    let f = fired.clone();
    rig.ws.on(WorkspaceEventName::Projection, move |e| {
        if let WorkspaceEvent::Projection { id } = e {
            f.borrow_mut().push(id.clone());
        }
    });
    rig.ws.cell("b").unwrap().set_price_style("line");
    assert!(
        fired.borrow().is_empty(),
        "an inactive cell does not re-project the chrome"
    );
    rig.ws.cell("a").unwrap().set_price_style("line");
    assert_eq!(fired.borrow().as_slice(), ["a"]);
    assert_eq!(rig.ws.projection().unwrap().price_style, "line");
    rig.ws.activate("b");
    assert_eq!(rig.ws.projection().unwrap().symbol, "BBB");
    assert_eq!(fired.borrow().last().map(String::as_str), Some("b"));
}

// ── layout changes ────────────────────────────────────────────

#[test]
fn shrinking_then_growing_keeps_surviving_charts_and_restores_dropped_cells() {
    let rig = quad();
    let a_before = rig.ws.cell("a").unwrap();
    let created = counter(&rig.ws, WorkspaceEventName::CellCreated);
    let destroyed = counter(&rig.ws, WorkspaceEventName::CellDestroyed);
    let layouts = counter(&rig.ws, WorkspaceEventName::LayoutChanged);
    // Give the cells that will be dropped some state worth restoring.
    rig.ws.cell("c").unwrap().set_price_style("line");
    rig.ws.cell("d").unwrap().set_timeframe("15");
    rig.settle();

    rig.ws.set_layout("2h").unwrap();
    rig.settle();
    assert_eq!(rig.ids(), ["a", "b"]);
    assert_eq!(destroyed.get(), 2);
    assert_eq!(layouts.get(), 1);
    assert_eq!(rig.host.destroyed(), ["c", "d"]);
    // The survivors are the SAME cells with the SAME live charts.
    assert!(a_before.is_same(&rig.ws.cell("a").unwrap()));

    rig.ws.set_layout("4").unwrap();
    rig.settle();
    assert_eq!(rig.ids(), ["a", "b", "c", "d"]);
    assert_eq!(created.get(), 2);
    // The re-created cells came back exactly as they were dropped.
    assert_eq!(rig.ws.cell("c").unwrap().symbol(), "CCC");
    assert_eq!(rig.ws.cell("c").unwrap().requested_price_style(), "line");
    assert_eq!(rig.ws.cell("d").unwrap().symbol(), "DDD");
    assert_eq!(rig.ws.cell("d").unwrap().timeframe(), "15");
    assert_eq!(chart_symbol(&rig.ws, "d").as_deref(), Some("DDD"));
}

#[test]
fn shrinking_never_pools_the_active_chart() {
    let rig = quad();
    rig.ws.activate("d");
    rig.ws.set_layout("1").unwrap();
    rig.settle();
    assert_eq!(
        rig.ids(),
        ["d"],
        "the active chart takes the last kept slot"
    );
    assert_eq!(rig.ws.active_id().as_deref(), Some("d"));
    rig.ws.set_layout("4").unwrap();
    rig.settle();
    assert_eq!(rig.ids(), ["d", "a", "b", "c"]);
}

#[test]
fn a_layout_change_that_drops_the_active_slot_falls_back_to_the_first() {
    let rig = Rig::new("4", &QUAD);
    rig.ws.activate("b");
    rig.ws.set_layout("2v").unwrap();
    rig.settle();
    assert_eq!(
        rig.ws.active_id().as_deref(),
        Some("b"),
        "b survives a 2 slot layout"
    );
    rig.ws.activate("a");
    rig.ws.set_layout("1").unwrap();
    rig.settle();
    assert_eq!(rig.ws.active_id().as_deref(), Some("a"));
}

#[test]
fn picker_grid_ids_synthesize_a_layout_and_the_workspace_builds_it() {
    let rig = quad();
    rig.ws.set_layout("g3x2").unwrap();
    rig.settle();
    assert_eq!(rig.ws.layout().id, "g3x2");
    assert_eq!(rig.ids().len(), 6);
    assert_eq!(rig.ids()[4..], ["c1", "c2"]);
    assert_eq!(
        rig.ws.layouts().definition("g3x2", None),
        None,
        "dynamic layouts stay unregistered"
    );
}

#[test]
fn single_chart_mode_pins_the_grid_to_one_cell() {
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts =
        crate::workspace::rig::options(&provider, "4", &[("only", "AAA"), ("two", "BBB")]);
    opts.layout = LayoutChoice::Single;
    let rig = Rig::with_options(platform, provider, opts);
    assert_eq!(rig.ids(), ["only"]);
    assert_eq!(rig.ws.layout().id, "1");
    rig.ws.set_layout("4").unwrap(); // a no-op
    assert_eq!(rig.ws.layout().id, "1");
    assert_eq!(rig.ids(), ["only"]);
}

#[test]
fn crossing_the_webgl_budget_rebuilds_every_cell_through_the_pool() {
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "4", &QUAD);
    opts.max_webgl_cells = 4;
    let rig = Rig::with_options(platform, provider, opts);
    let before = rig.ws.cell("a").unwrap();
    rig.ws.set_layout("g3x2").unwrap(); // 6 cells > 4: every cell flips to canvas2d
    rig.settle();
    assert_eq!(rig.ids().len(), 6);
    assert!(
        !before.is_same(&rig.ws.cell("a").unwrap()),
        "the backend flip rebuilt the cell"
    );
    assert_eq!(
        rig.ws.cell("a").unwrap().symbol(),
        "AAA",
        "through the pool, so its state survived"
    );
}

// ── maximize, swap, tracks ────────────────────────────────────

#[test]
fn maximize_is_presentation_only_and_a_layout_switch_restores_it() {
    let rig = quad();
    let events = Rc::new(RefCell::new(Vec::<Option<String>>::new()));
    let e = events.clone();
    rig.ws.on(WorkspaceEventName::CellMaximized, move |ev| {
        if let WorkspaceEvent::CellMaximized { id } = ev {
            e.borrow_mut().push(id.clone());
        }
    });
    rig.ws.maximize_cell(Some("c"));
    assert_eq!(rig.ws.maximized_cell().as_deref(), Some("c"));
    assert_eq!(
        rig.ws.active_id().as_deref(),
        Some("c"),
        "the maximized cell becomes active"
    );
    let grid = rig.host.last_grid().unwrap();
    assert_eq!(grid.maximized.as_deref(), Some("c"));
    let c = grid.cells.iter().find(|p| p.id == "c").unwrap();
    assert_eq!((c.z_index, c.hidden), (Some(5), false));
    assert_eq!(c.grid_area.as_deref(), Some("1 / 1 / -1 / -1"));
    let a = grid.cells.iter().find(|p| p.id == "a").unwrap();
    assert!(a.hidden && a.grid_area.as_deref() == Some("1 / 1 / -1 / -1"));
    assert_eq!(rig.ids().len(), 4, "the other cells stay alive underneath");
    rig.ws.maximize_cell(Some("nope"));
    assert_eq!(rig.ws.maximized_cell().as_deref(), Some("c"));
    rig.ws.set_layout("2h").unwrap();
    assert_eq!(rig.ws.maximized_cell(), None);
    assert_eq!(events.borrow().as_slice(), [Some("c".to_string()), None]);
}

#[test]
fn a_single_cell_layout_has_nothing_to_maximize() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    rig.ws.maximize_cell(Some("only"));
    assert_eq!(rig.ws.maximized_cell(), None);
}

#[test]
fn swapping_two_cells_trades_their_slots_and_keeps_their_charts() {
    let rig = quad();
    let b = rig.ws.cell("b").unwrap();
    rig.ws.swap_cells("b", "d");
    assert_eq!(rig.ids(), ["a", "d", "c", "b"]);
    assert!(b.is_same(&rig.ws.cell("b").unwrap()));
    let grid = rig.host.last_grid().unwrap();
    let order: Vec<&str> = grid.cells.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, ["a", "d", "c", "b"]);
    rig.ws.swap_cells("a", "a");
    rig.ws.swap_cells("a", "nope");
    assert_eq!(rig.ids(), ["a", "d", "c", "b"]);
    // The swapped arrangement is what the document saves.
    let saved: Vec<String> = rig
        .ws
        .get_state()
        .charts
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(saved, ["a", "d", "c", "b"]);
}

#[test]
fn splitter_drags_change_the_tracks_and_persist_per_layout() {
    let rig = quad();
    let strips = rig.ws.splitter_strips(800.0, 400.0);
    assert_eq!(strips.len(), 2);
    rig.ws.apply_tracks(Axis::Cols, vec![1.5, 0.5]);
    assert_eq!(rig.ws.tracks().0, [1.5, 0.5]);
    assert_eq!(rig.host.last_grid().unwrap().cols, [1.5, 0.5]);
    let state = rig.ws.get_state();
    assert_eq!(
        state.track_sizes.as_ref().unwrap()["4"].cols.as_deref(),
        Some(&[1.5, 0.5][..])
    );
    // Another layout has its own tracks.
    rig.ws.set_layout("2h").unwrap();
    assert_eq!(rig.ws.tracks().0, [1.0, 1.0]);
    rig.ws.set_layout("4").unwrap();
    assert_eq!(rig.ws.tracks().0, [1.5, 0.5]);
    rig.ws.reset_tracks(Axis::Cols);
    assert_eq!(rig.ws.tracks().0, [1.0, 1.0]);
}

// ── sync ──────────────────────────────────────────────────────

#[test]
fn symbol_sync_off_leaves_the_other_cells_alone() {
    let rig = quad();
    rig.ws.cell("a").unwrap().set_symbol("EEE");
    rig.settle();
    assert_eq!(rig.symbols(), ["EEE", "BBB", "CCC", "DDD"]);
}

#[test]
fn symbol_sync_on_aligns_the_followers_once_and_then_follows_every_switch() {
    let rig = quad();
    rig.ws
        .sync_set(SyncKind::Symbol, Some(SyncSetting::Flag(true)));
    rig.settle();
    assert_eq!(
        rig.symbols(),
        ["AAA"; 4],
        "enabling aligns the followers to the active cell"
    );
    rig.ws.cell("c").unwrap().set_symbol("EEE");
    rig.settle();
    assert_eq!(rig.symbols(), ["EEE"; 4], "any cell is an origin");
    for id in ["a", "b", "c", "d"] {
        assert_eq!(
            chart_symbol(&rig.ws, id).as_deref(),
            Some("EEE"),
            "the charts moved too"
        );
    }
    rig.ws.sync_set(SyncKind::Symbol, None);
    rig.ws.cell("b").unwrap().set_symbol("BBB");
    rig.settle();
    assert_eq!(
        rig.symbols(),
        ["EEE", "BBB", "EEE", "EEE"],
        "off again: nothing follows"
    );
    assert_eq!(rig.ws.sync_get(SyncKind::Symbol), None);
}

#[test]
fn symbol_sync_by_group_only_links_same_group_cells() {
    let rig = quad();
    let groups = SyncSetting::Groups(
        [("a", "x"), ("b", "x"), ("c", "y")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    rig.ws.sync_set(SyncKind::Symbol, Some(groups));
    rig.settle();
    assert_eq!(
        rig.symbols(),
        ["AAA", "AAA", "CCC", "DDD"],
        "b joined a's group; c and d untouched"
    );
    rig.ws.cell("b").unwrap().set_symbol("EEE");
    rig.settle();
    assert_eq!(rig.symbols(), ["EEE", "EEE", "CCC", "DDD"]);
}

#[test]
fn timeframe_sync_links_the_requested_timeframe() {
    let rig = quad();
    rig.ws
        .sync_set(SyncKind::Timeframe, Some(SyncSetting::Flag(true)));
    rig.ws.cell("b").unwrap().set_timeframe("15");
    rig.settle();
    for cell in rig.ws.cells() {
        assert_eq!(cell.timeframe(), "15", "{} follows", cell.id());
    }
    assert!(rig.provider.calls.borrow().iter().any(|(_, tf)| tf == "15"));
}

#[test]
fn toggling_a_sync_row_flips_between_all_cells_and_off() {
    let rig = quad();
    let rows = rig.ws.sync_rows();
    let ids: Vec<&str> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, ["symbol", "timeframe", "crosshair", "style"]);
    assert_eq!(rows[0].label, "Symbol");
    assert_eq!(rows[1].label, "Interval");
    assert!(rows.iter().all(|r| !r.checked));
    rig.ws.toggle_sync(SyncKind::Crosshair);
    assert!(rig.ws.sync_rows()[2].checked);
    assert_eq!(
        rig.ws.sync_get(SyncKind::Crosshair),
        Some(SyncSetting::Flag(true))
    );
    rig.ws.toggle_sync(SyncKind::Crosshair);
    assert!(!rig.ws.sync_rows()[2].checked);
    // A host-set group record reads as on, and a flip turns it off.
    rig.ws.sync_set(
        SyncKind::Style,
        Some(SyncSetting::Groups(
            [("a".to_string(), "g".to_string())].into(),
        )),
    );
    rig.ws.toggle_sync(SyncKind::Style);
    assert_eq!(rig.ws.sync_get(SyncKind::Style), None);
}

/// The ghost crosshair a cell's renderer shows now: time and price.
fn ghost_of(rig: &Rig, id: &str) -> Option<(i64, Option<f64>)> {
    rig.ws.cell(id)?.renderer()?.with(|st| st.external_cross)
}

/// Put the pointer of cell `id` over its bar `index` (a price pane height); `None` leaves.
fn hover(rig: &Rig, id: &str, index: Option<f64>) -> i64 {
    let r = rig.ws.cell(id).unwrap().renderer().unwrap();
    match index {
        Some(i) => r.pointer_move(r.coordinates().logical_to_x(i), 100.0),
        None => r.pointer_leave(),
    }
    T0 + index.unwrap_or(0.0) as i64 * 3_600_000
}

#[test]
fn crosshair_sync_mirrors_the_pointer_time_onto_followers_only_while_linked() {
    let rig = quad();
    // Off: no ghosts.
    hover(&rig, "a", Some(290.0));
    assert_eq!(ghost_of(&rig, "b"), None);
    rig.ws
        .sync_set(SyncKind::Crosshair, Some(SyncSetting::Flag(true)));
    let t = hover(&rig, "a", Some(290.0));
    for id in ["b", "c", "d"] {
        assert_eq!(
            ghost_of(&rig, id).map(|g| g.0),
            Some(t),
            "{id} shows the ghost"
        );
    }
    assert_eq!(
        ghost_of(&rig, "a"),
        None,
        "the origin shows its own crosshair"
    );
    hover(&rig, "a", None); // leaving the origin clears every ghost
    assert_eq!(ghost_of(&rig, "b"), None);
    // Turning the link off clears what is up.
    hover(&rig, "a", Some(290.0));
    assert!(ghost_of(&rig, "b").is_some());
    rig.ws.sync_set(SyncKind::Crosshair, None);
    assert_eq!(ghost_of(&rig, "b"), None);
    hover(&rig, "a", Some(290.0));
    assert_eq!(ghost_of(&rig, "b"), None);
}

#[test]
fn crosshair_price_travels_only_to_followers_on_the_same_ticker() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "AAA")]);
    rig.ws
        .sync_set(SyncKind::Crosshair, Some(SyncSetting::Flag(true)));
    hover(&rig, "a", Some(290.0));
    assert!(
        ghost_of(&rig, "b").is_some_and(|g| g.1.is_some()),
        "same ticker: the level rides along"
    );
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    rig.ws
        .sync_set(SyncKind::Crosshair, Some(SyncSetting::Flag(true)));
    hover(&rig, "a", Some(290.0));
    let ghost = ghost_of(&rig, "b").expect("a ghost");
    assert_eq!(ghost.1, None, "another market's price would be noise");
}

#[test]
fn viewport_sync_pushes_the_visible_range_onto_the_followers() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    rig.ws
        .sync_set(SyncKind::Viewport, Some(SyncSetting::Flag(true)));
    let a = rig.ws.cell("a").unwrap().try_chart().unwrap();
    let b = rig.ws.cell("b").unwrap().try_chart().unwrap();
    let from = T0 + 100 * 3_600_000;
    let to = T0 + 160 * 3_600_000;
    a.set_visible_range(crate::ports::chart_renderer::VisibleRange { from, to });
    rig.settle();
    let r = b.get_visible_range().expect("a range");
    assert!(
        (r.from - from).abs() <= 1_800_000 && (r.to - to).abs() <= 1_800_000,
        "{r:?}"
    );
}

// ── drawings sync ─────────────────────────────────────────────

fn trendline(chart: &crate::vela::Vela, price: f64) -> String {
    let init = crate::drawings::controller::AddInit {
        anchors: Some(vec![
            crate::drawings::DrawingPoint {
                time: (T0 + 3_600_000) as f64,
                price,
            },
            crate::drawings::DrawingPoint {
                time: (T0 + 20 * 3_600_000) as f64,
                price: price + 1.0,
            },
        ]),
        ..Default::default()
    };
    chart
        .drawings()
        .add("trendline", init)
        .expect("a drawing")
        .id
}

fn drawing_count(ws: &Workspace, id: &str) -> usize {
    ws.cell(id)
        .unwrap()
        .try_chart()
        .unwrap()
        .drawings()
        .all()
        .len()
}

#[test]
fn a_created_drawing_copies_onto_same_group_cells_and_edits_and_removals_follow() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap().try_chart().unwrap();
    let b = rig.ws.cell("b").unwrap().try_chart().unwrap();
    // Off: independent.
    trendline(&a, 100.0);
    assert_eq!(
        (drawing_count(&rig.ws, "a"), drawing_count(&rig.ws, "b")),
        (1, 0)
    );
    rig.ws
        .sync_set(SyncKind::Drawings, Some(SyncSetting::Flag(true)));
    let id = trendline(&a, 110.0);
    assert_eq!(
        drawing_count(&rig.ws, "b"),
        1,
        "the new drawing copied across"
    );
    let copy = b.drawings().all()[0].clone();
    assert_eq!(
        copy.anchors,
        a.drawings()
            .all()
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .anchors
    );
    // An edit follows.
    a.drawings().update(
        &id,
        crate::drawings::controller::DrawingUpdate {
            anchors: Some(vec![
                crate::drawings::DrawingPoint {
                    time: (T0 + 2 * 3_600_000) as f64,
                    price: 120.0,
                },
                crate::drawings::DrawingPoint {
                    time: (T0 + 30 * 3_600_000) as f64,
                    price: 125.0,
                },
            ]),
            ..Default::default()
        },
    );
    assert_eq!(b.drawings().all()[0].anchors[0].price, 120.0);
    // A removal follows.
    a.drawings().remove(&id);
    assert_eq!(drawing_count(&rig.ws, "b"), 0);
    assert_eq!(
        drawing_count(&rig.ws, "a"),
        1,
        "the unlinked first drawing stays"
    );
}

// ── style sync ────────────────────────────────────────────────

#[test]
fn style_sync_mirrors_looks_and_status_prefs_onto_the_followers() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap();
    let b = rig.ws.cell("b").unwrap();
    // Off: a look does not travel.
    a.set_watermark_visible(false);
    assert!(b.status_prefs().watermark);
    rig.ws
        .sync_set(SyncKind::Style, Some(SyncSetting::Flag(true)));
    assert!(
        !b.status_prefs().watermark,
        "enabling aligns the followers to the active cell"
    );
    a.set_indicator_titles_visible(false);
    assert!(!b.status_prefs().indicator_titles);
    b.set_statusline_part(crate::workspace::cell::StatuslinePart::Ohlc, false);
    assert!(
        !a.status_prefs().parts.unwrap().ohlc,
        "any cell is an origin"
    );
    // A renderer look travels as a config block.
    let ra = a.try_chart().unwrap();
    ra.renderer()
        .apply_config(&json!({ "candles": { "upColor": "#123456" } }));
    let rb = b.try_chart().unwrap().renderer().get_config().unwrap();
    assert_eq!(rb["candles"]["upColor"], "#123456");
}

// ── events, alerts, toasts ────────────────────────────────────

#[test]
fn a_cell_price_style_change_is_relayed_with_its_identity() {
    let rig = quad();
    let seen: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let s = seen.clone();
    rig.ws.on(WorkspaceEventName::CellPriceStyle, move |e| {
        if let WorkspaceEvent::CellPriceStyle { id, from, to } = e {
            s.borrow_mut().push((id.clone(), from.clone(), to.clone()));
        }
    });
    rig.ws.cell("c").unwrap().set_price_style("line");
    assert_eq!(
        seen.borrow().as_slice(),
        [("c".to_string(), "candles".to_string(), "line".to_string())]
    );
}

#[test]
fn an_unresolved_symbol_toasts_once_per_symbol() {
    // A symbol nothing serves parks the load; the core reports it once every provider index
    // settled, which is what the cell turns into one notice.
    let rig = Rig::new("2h", &[("a", "NOWHERE"), ("b", "BBB")]);
    rig.platform.advance(1_000.0);
    rig.settle();
    let toasts = rig.host.toasts();
    let hits: Vec<&(String, ToastKind)> = toasts
        .iter()
        .filter(|(m, _)| m.contains("No registered provider serves"))
        .collect();
    assert_eq!(hits.len(), 1, "{toasts:?}");
    assert_eq!(hits[0].1, ToastKind::Error);
    assert!(hits[0].0.contains("NOWHERE") && hits[0].0.contains("fake"));
}

// ── state surface, persistence ────────────────────────────────

#[test]
fn get_state_lists_the_slots_in_order_with_the_shared_prefs() {
    let rig = quad();
    rig.ws.activate("c");
    rig.ws.set_timezone("America/New_York");
    rig.ws.set_timeframe_favorite("15", true);
    rig.ws
        .sync_set(SyncKind::Crosshair, Some(SyncSetting::Flag(true)));
    let state = rig.ws.get_state();
    assert_eq!(
        state.sync.as_ref().unwrap().crosshair,
        Some(SyncSetting::Flag(true))
    );
    assert_eq!(state.version, 1);
    assert_eq!(state.layout, "4");
    let ids: Vec<&str> = state.charts.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
    assert_eq!(state.charts[1].cell.symbol.as_deref(), Some("BBB"));
    assert_eq!(state.active_cell_id.as_deref(), Some("c"));
    assert_eq!(state.timezone.as_deref(), Some("America/New_York"));
    assert_eq!(
        state.timeframe_favorites.as_deref(),
        Some(&["15".to_string()][..])
    );
    assert!(state.charts[0].cell.renderer_config.is_some());
    assert!(state.charts[0].cell.indicators.is_some());
}

#[test]
fn save_destroy_restore_round_trips_a_whole_workspace() {
    let storage = memory_storage_adapter();
    let rig = Rig::persisted("4", &QUAD, Rc::new(storage.clone()), "ws");
    rig.ws.cell("b").unwrap().set_timeframe("15");
    rig.ws.cell("c").unwrap().set_price_style("line");
    rig.ws.activate("d");
    rig.ws.apply_tracks(Axis::Rows, vec![1.2, 0.8]);
    let a = rig.ws.cell("a").unwrap().try_chart().unwrap();
    trendline(&a, 100.0);
    rig.ws.set_timezone("Europe/Paris");
    rig.settle();
    let saved = rig.ws.get_state();
    rig.ws.destroy();
    assert!(rig.ws.is_destroyed());
    // Destroy flushed the pending edit through the storage.
    let StorageRead::Ready(Some(raw)) = storage.get("ws") else {
        panic!("destroy must flush the debounced write");
    };

    // A fresh workspace over the same storage restores everything with NO declared cells.
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "2h", &[]);
    opts.persist = Some("ws".into());
    opts.storage = Some(Rc::new(storage.clone()));
    let rig2 = Rig::with_options(platform, provider, opts);
    assert_eq!(
        rig2.ws.layout().id,
        "4",
        "the document's layout wins over the option"
    );
    assert_eq!(rig2.ids(), ["a", "b", "c", "d"]);
    assert_eq!(rig2.symbols(), ["AAA", "BBB", "CCC", "DDD"]);
    assert_eq!(rig2.ws.cell("b").unwrap().timeframe(), "15");
    assert_eq!(rig2.ws.cell("c").unwrap().requested_price_style(), "line");
    assert_eq!(rig2.ws.active_id().as_deref(), Some("d"));
    assert_eq!(rig2.ws.tracks().1, [1.2, 0.8]);
    assert_eq!(rig2.ws.timezone(), "Europe/Paris");
    assert_eq!(drawing_count(&rig2.ws, "a"), 1, "drawings round-tripped");
    let again = rig2.ws.get_state();
    assert_eq!(again.charts.len(), saved.charts.len());
    for (x, y) in saved.charts.iter().zip(&again.charts) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.cell.symbol, y.cell.symbol);
        assert_eq!(x.cell.timeframe, y.cell.timeframe);
        assert_eq!(x.cell.price_style, y.cell.price_style);
    }
    let _ = raw;
}

#[test]
fn apply_state_in_place_keeps_the_chart_instances() {
    let rig = quad();
    let a_chart = rig.ws.cell("a").unwrap().try_chart().unwrap();
    let mut doc = rig.ws.get_state();
    doc.charts[0].cell.symbol = Some("EEE".into());
    doc.charts[1].cell.timeframe = Some("15".into());
    doc.charts[2].cell.price_style = Some("line".into());
    // The renderer config carries the style too and is applied after it: edit one or the other.
    doc.charts[2].cell.renderer_config = None;
    doc.active_cell_id = Some("b".into());
    let created = counter(&rig.ws, WorkspaceEventName::CellCreated);
    let layouts = counter(&rig.ws, WorkspaceEventName::LayoutChanged);
    rig.ws.apply_state(&serde_json::to_value(&doc).unwrap());
    rig.settle();
    assert_eq!(
        created.get() + layouts.get(),
        0,
        "structurally nothing happened"
    );
    assert_eq!(rig.ws.cell("a").unwrap().symbol(), "EEE");
    assert_eq!(chart_symbol(&rig.ws, "a").as_deref(), Some("EEE"));
    assert_eq!(rig.ws.cell("b").unwrap().timeframe(), "15");
    assert_eq!(rig.ws.cell("c").unwrap().requested_price_style(), "line");
    assert_eq!(rig.ws.active_id().as_deref(), Some("b"));
    assert!(
        a_chart.market().symbol.as_deref() == Some("EEE"),
        "the same chart instance switched in place"
    );
    assert!(rig.ws.cell("a").unwrap().try_chart().unwrap().market() == a_chart.market());
}

#[test]
fn apply_state_with_another_shape_rebuilds_from_the_document() {
    let rig = quad();
    let mut doc = rig.ws.get_state();
    doc.layout = "2h".into();
    doc.charts.truncate(2);
    doc.charts[0].id = "left".into();
    doc.charts[1].id = "right".into();
    doc.charts[0].cell.symbol = Some("EEE".into());
    doc.active_cell_id = Some("right".into());
    let destroyed = counter(&rig.ws, WorkspaceEventName::CellDestroyed);
    let layouts = counter(&rig.ws, WorkspaceEventName::LayoutChanged);
    rig.ws.apply_state(&serde_json::to_value(&doc).unwrap());
    rig.settle();
    assert_eq!(rig.ids(), ["left", "right"]);
    assert_eq!(destroyed.get(), 4);
    assert_eq!(layouts.get(), 1);
    assert_eq!(rig.ws.layout().id, "2h");
    assert_eq!(rig.ws.cell("left").unwrap().symbol(), "EEE");
    assert_eq!(rig.ws.active_id().as_deref(), Some("right"));
}

#[test]
fn apply_state_ignores_garbage_and_an_unregistered_layout_keeps_the_grid() {
    let rig = quad();
    rig.ws.apply_state(&json!("garbage"));
    rig.ws.apply_state(&json!({ "version": 99 }));
    rig.ws.apply_state(&Value::Null);
    assert_eq!(rig.ids(), ["a", "b", "c", "d"]);
    let mut doc = rig.ws.get_state();
    doc.layout = "never-registered".into();
    rig.ws.apply_state(&serde_json::to_value(&doc).unwrap());
    rig.settle();
    assert_eq!(rig.ws.layout().id, "4");
    assert_eq!(rig.ids(), ["a", "b", "c", "d"]);
}

#[test]
fn state_changed_is_debounced_and_boot_is_not_an_edit() {
    let rig = quad();
    let changed = counter(&rig.ws, WorkspaceEventName::StateChanged);
    rig.platform.advance(2_000.0);
    assert_eq!(changed.get(), 0, "boot is not an edit");
    rig.ws.cell("a").unwrap().set_price_style("line");
    rig.ws.cell("b").unwrap().set_price_style("line");
    rig.ws.set_timezone("Asia/Tokyo");
    assert_eq!(changed.get(), 0, "inside the debounce window");
    rig.platform.advance(499.0);
    assert_eq!(changed.get(), 0);
    rig.platform.advance(2.0);
    assert_eq!(changed.get(), 1, "one notification per burst");
}

#[test]
fn destroy_flushes_the_pending_edit_once_and_reports_what_was_on_screen() {
    let storage = MemoryStorage::new();
    let rig = Rig::persisted("4", &QUAD, Rc::new(storage.clone()), "ws");
    let changed = counter(&rig.ws, WorkspaceEventName::StateChanged);
    rig.ws.cell("c").unwrap().set_symbol("EEE");
    rig.ws.destroy();
    assert_eq!(changed.get(), 1, "the last edit reached the host");
    rig.ws.destroy(); // idempotent
    assert_eq!(changed.get(), 1);
    let StorageRead::Ready(Some(raw)) = storage.get("ws") else {
        panic!("a write");
    };
    let doc = crate::state::decode_state(&raw).expect("a document");
    assert_eq!(doc.charts[2].cell.symbol.as_deref(), Some("EEE"));
    // State read after destroy is what was on screen, not the last applied document.
    assert_eq!(
        rig.ws.get_state().charts[2].cell.symbol.as_deref(),
        Some("EEE")
    );
    assert_eq!(rig.host.destroyed().len(), 4);
}

#[test]
fn a_teardown_with_nothing_pending_stays_silent() {
    let storage = MemoryStorage::new();
    let rig = Rig::persisted("4", &QUAD, Rc::new(storage.clone()), "ws");
    let changed = counter(&rig.ws, WorkspaceEventName::StateChanged);
    rig.ws.destroy();
    assert_eq!(changed.get(), 0);
    assert!(
        matches!(storage.get("ws"), StorageRead::Ready(None)),
        "nothing was invented"
    );
}

struct LateStorage {
    inner: MemoryStorage,
}

impl WorkspaceStorage for LateStorage {
    fn get(&self, key: &str) -> StorageRead {
        let StorageRead::Ready(value) = self.inner.get(key) else {
            return StorageRead::Ready(None);
        };
        StorageRead::Pending(Box::pin(async move { value }))
    }

    fn set(&self, key: &str, value: &str) {
        self.inner.set(key, value);
    }

    fn remove(&self, key: &str) {
        self.inner.remove(key);
    }
}

#[test]
fn an_async_storage_late_applies_through_apply_state() {
    let first = MemoryStorage::new();
    let rig = Rig::persisted("4", &QUAD, Rc::new(first.clone()), "ws");
    rig.ws.cell("a").unwrap().set_symbol("EEE");
    rig.ws.destroy();
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "4", &QUAD);
    opts.persist = Some("ws".into());
    opts.storage = Some(Rc::new(LateStorage { inner: first }));
    let rig2 = Rig::with_options(platform, provider, opts);
    rig2.settle();
    assert_eq!(
        rig2.ws.cell("a").unwrap().symbol(),
        "EEE",
        "the stored document landed after boot"
    );
}

#[test]
fn garbage_in_storage_boots_the_defaults() {
    let storage = MemoryStorage::new();
    storage.set("ws", "{not json");
    let rig = Rig::persisted("4", &QUAD, Rc::new(storage), "ws");
    assert_eq!(rig.symbols(), ["AAA", "BBB", "CCC", "DDD"]);
}

// ── theme, locale, timezone ───────────────────────────────────

#[test]
fn set_theme_reaches_every_cell_and_sticks_for_later_cells() {
    let rig = quad();
    rig.ws.set_theme("light");
    for cell in rig.ws.cells() {
        let theme = cell.renderer().unwrap().with(|st| st.theme.clone());
        assert_eq!(
            theme["background"],
            crate::theme::light_theme()["background"]
        );
    }
    rig.ws.set_layout("2h").unwrap();
    rig.ws.set_layout("4").unwrap();
    rig.settle();
    let rebuilt = rig
        .ws
        .cell("c")
        .unwrap()
        .renderer()
        .unwrap()
        .with(|st| st.theme.clone());
    assert_eq!(
        rebuilt["background"],
        crate::theme::light_theme()["background"]
    );
}

#[test]
fn set_locale_re_renders_every_cell_and_fires_once() {
    let rig = quad();
    let fired: Rc<RefCell<Vec<String>>> = Rc::default();
    let f = fired.clone();
    rig.ws.on(WorkspaceEventName::LocaleChanged, move |e| {
        if let WorkspaceEvent::LocaleChanged(l) = e {
            f.borrow_mut().push(l.locale.clone());
        }
    });
    assert_eq!(rig.ws.i18n().locale(), "en");
    rig.ws.set_locale("de");
    assert_eq!(fired.borrow().as_slice(), ["de"]);
    assert_eq!(rig.ws.i18n().locale(), "de");
    for cell in rig.ws.cells() {
        assert_eq!(cell.try_chart().unwrap().i18n().locale(), "de");
    }
    // A cell built later starts in the workspace's language.
    rig.ws.set_layout("g3x2").unwrap();
    rig.settle();
    assert_eq!(
        rig.ws
            .cell("c1")
            .unwrap()
            .try_chart()
            .unwrap()
            .i18n()
            .locale(),
        "de"
    );
}

#[test]
fn the_timezone_choice_reaches_every_cell_and_the_exchange_rule_resolves_per_cell() {
    let rig = quad();
    rig.ws.set_timezone("Asia/Tokyo");
    for cell in rig.ws.cells() {
        let zone = cell.try_chart().unwrap().renderer().get("timezone");
        assert_eq!(zone, Some(json!("Asia/Tokyo")));
    }
    rig.ws.set_timezone("exchange");
    // No metadata yet: the rule renders UTC.
    assert_eq!(rig.ws.cell("a").unwrap().display_timezone(), "Etc/UTC");
}

#[test]
fn the_workspace_context_resolves_live_to_the_active_cell() {
    let rig = quad();
    let ctx = rig.ws.context();
    assert_eq!(ctx.symbol(), "AAA");
    assert_eq!(ctx.active_cell_id(), "a");
    assert_eq!(ctx.cells().len(), 4);
    rig.ws.activate("c");
    assert_eq!(ctx.symbol(), "CCC", "the SAME context follows the switch");
    assert_eq!(ctx.active_cell_id(), "c");
    ctx.set_symbol("EEE");
    rig.settle();
    assert_eq!(rig.ws.cell("c").unwrap().symbol(), "EEE");
    ctx.set_active_cell("b");
    assert_eq!(ctx.timeframe(), "60");
    assert_eq!(ctx.price_style(), "candles");
    assert!(ctx.chart().is_some() && ctx.replay().is_some() && ctx.i18n().is_some());
}

#[test]
fn a_destroyed_workspace_refuses_further_work() {
    let rig = quad();
    let ctx = rig.ws.context();
    rig.ws.destroy();
    assert!(rig.ws.cells().is_empty());
    assert!(ctx.chart().is_none());
    assert_eq!(ctx.symbol(), "");
    rig.ws.set_layout("2h").unwrap();
    rig.ws.set_active_cell(Some("a"));
    rig.ws.set_locale("fr");
    assert!(rig.ws.cells().is_empty());
}

// ── pure helpers ──────────────────────────────────────────────

#[test]
fn declaration_order_is_the_slot_order_and_numeric_names_are_rejected() {
    let seeds = |names: &[&str]| -> Vec<(String, CellSeed)> {
        names
            .iter()
            .map(|n| (n.to_string(), CellSeed::default()))
            .collect()
    };
    assert_eq!(
        declared_order(&seeds(&["btc", "eth", "sol"]), None),
        strings(&["btc", "eth", "sol"])
    );
    assert!(declared_order(&[], None).is_empty());
    let platform = crate::runtime::testing::ManualPlatform::new(0.0);
    let handle = platform.handle();
    assert_eq!(
        declared_order(&seeds(&["btc", "2", "eth"]), Some(handle.as_ref())),
        strings(&["btc", "eth"])
    );
    assert!(platform.warnings().iter().any(|w| w.contains("\"2\"")));
}

#[test]
fn auto_identities_pick_the_first_free_canonical_name() {
    let set = |v: &[&str]| -> std::collections::HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    };
    assert_eq!(next_auto_cell_id(&set(&[])), "c1");
    assert_eq!(next_auto_cell_id(&set(&["c1", "c2"])), "c3");
    assert_eq!(next_auto_cell_id(&set(&["btc", "c1", "c3"])), "c2");
}

#[test]
fn resolve_typing_routes_bare_keystrokes() {
    let idle = TypingState {
        dialogs: 0,
        symbol_search: false,
        timeframe_entry: false,
    };
    let sym = |t: &str| Some((TypingTarget::Symbol, t.to_string()));
    let tf = |t: &str| Some((TypingTarget::Timeframe, t.to_string()));
    assert_eq!(resolve_typing("n", idle), sym("N"));
    assert_eq!(resolve_typing("Q", idle), sym("Q"));
    assert_eq!(resolve_typing("0", idle), sym("0"));
    assert_eq!(resolve_typing("1", idle), tf("1"));
    assert_eq!(resolve_typing("9", idle), tf("9"));
    assert_eq!(resolve_typing("Escape", idle), None);
    assert_eq!(resolve_typing(" ", idle), None);
    assert_eq!(
        resolve_typing("\u{e9}", idle),
        sym("\u{c9}"),
        "a letter of any script opens symbol search"
    );
    let search = TypingState {
        dialogs: 1,
        symbol_search: true,
        timeframe_entry: false,
    };
    assert_eq!(resolve_typing("q", search), sym("Q"));
    assert_eq!(resolve_typing("2", search), sym("2"));
    assert_eq!(resolve_typing("Enter", search), None);
    let entry = TypingState {
        dialogs: 1,
        symbol_search: false,
        timeframe_entry: true,
    };
    assert_eq!(resolve_typing("5", entry), tf("5"));
    assert_eq!(resolve_typing("0", entry), tf("0"));
    assert_eq!(resolve_typing("m", entry), tf("M"));
    let other = TypingState {
        dialogs: 2,
        symbol_search: false,
        timeframe_entry: false,
    };
    assert_eq!(resolve_typing("n", other), None);
    assert_eq!(resolve_typing("1", other), None);
}

#[test]
fn the_layout_mode_reaches_every_cell_and_cells_born_later() {
    use crate::ports::chart_renderer::LayoutMode;
    let rig = quad();
    assert_eq!(rig.ws.layout_mode(), LayoutMode::Desktop);
    rig.ws.set_layout_mode(LayoutMode::Mobile);
    assert_eq!(rig.ws.layout_mode(), LayoutMode::Mobile);
    for id in ["a", "b", "c", "d"] {
        let modes = rig.host.cell_host(id).unwrap().layout_modes();
        assert_eq!(modes.last(), Some(&LayoutMode::Mobile), "{id}");
    }
    rig.ws.set_layout("g3x2").unwrap();
    rig.settle();
    let born = rig.host.cell_host("c1").unwrap().layout_modes();
    assert_eq!(
        born.last(),
        Some(&LayoutMode::Mobile),
        "a newborn cell starts in the live mode"
    );
}

// ── seams the browser shell leans on ──────────────────────────

#[test]
fn a_single_visible_cell_screenshot_goes_through_the_hosts_surface() {
    use crate::workspace::screenshot::{CellBox, GridBox};
    let rig = Rig::new("1", &[("a", "AAA")]);
    rig.host.set_grid(
        GridBox {
            left: 0.0,
            top: 0.0,
            width: 800.0,
            height: 400.0,
        },
        2.0,
    );
    rig.host.set_cell_box(
        "a",
        CellBox {
            hidden: false,
            left: 0.0,
            top: 0.0,
            width: 800.0,
            height: 400.0,
        },
    );
    let url = rig.ws.screenshot();
    assert_eq!(url.as_deref(), Some("data:image/png;base64,LAYOUT"));
    let surfaces = rig.host.surfaces();
    assert_eq!(surfaces.len(), 1, "one composite, no renderer export");
    assert_eq!(surfaces[0].size(), (1600.0, 800.0), "the cell's box at 2x");
    use crate::workspace::screenshot::OverlayPhase::{Over, Under};
    assert_eq!(
        surfaces[0].overlays(),
        [("a".to_string(), Under), ("a".to_string(), Over)],
        "the cell's DOM overlays join its tile, under then over"
    );
}

#[test]
fn a_maximized_cell_of_a_grid_is_a_single_cell_shot() {
    use crate::workspace::screenshot::CellBox;
    let rig = quad();
    rig.host.set_cell_box(
        "b",
        CellBox {
            hidden: false,
            left: 0.0,
            top: 0.0,
            width: 640.0,
            height: 360.0,
        },
    );
    rig.ws.maximize_cell(Some("b"));
    assert!(rig.ws.screenshot().is_some());
    let surfaces = rig.host.surfaces();
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].size(), (640.0, 360.0));
}

#[test]
fn the_host_receives_each_cell_with_its_chart_alive() {
    let rig = quad();
    assert_eq!(
        rig.host.ready(),
        [
            ("a".to_string(), true),
            ("b".to_string(), true),
            ("c".to_string(), true),
            ("d".to_string(), true)
        ]
    );
}

#[test]
fn a_status_prefs_change_raises_an_event_on_any_cell_active_or_not() {
    let rig = quad();
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let s = seen.clone();
    rig.ws.on(WorkspaceEventName::StatusPrefs, move |ev| {
        if let WorkspaceEvent::StatusPrefs { id } = ev {
            s.borrow_mut().push(id.clone());
        }
    });
    assert_eq!(rig.ws.active_id().as_deref(), Some("a"));
    rig.ws.cell("c").unwrap().set_watermark_visible(false);
    assert_eq!(*seen.borrow(), ["c"], "a cell that is not active");
    rig.ws
        .cell("a")
        .unwrap()
        .set_indicator_titles_visible(false);
    assert_eq!(*seen.borrow(), ["c", "a"]);
    // The style link mirrors onto the followers, and each follower reports too.
    rig.ws
        .sync_set(SyncKind::Style, Some(SyncSetting::Flag(true)));
    seen.borrow_mut().clear();
    rig.ws.cell("a").unwrap().set_watermark_visible(false);
    let mut ids = seen.borrow().clone();
    ids.sort();
    ids.dedup();
    assert_eq!(ids, ["a", "b", "c", "d"]);
}

#[test]
fn apply_state_raises_panels_restored_in_place_and_on_a_rebuild() {
    use crate::state::PanelsState;
    let rig = quad();
    let restored = counter(&rig.ws, WorkspaceEventName::PanelsRestored);
    let mut doc = rig.ws.get_state();
    doc.panels = Some(PanelsState {
        open: Some("objects".into()),
        ..PanelsState::default()
    });
    rig.ws.apply_state(&serde_json::to_value(&doc).unwrap());
    assert_eq!(restored.get(), 1, "in place");
    assert_eq!(
        rig.ws.panels_state().and_then(|p| p.open).as_deref(),
        Some("objects"),
        "the slice is readable when the event fires"
    );
    doc.layout = "2h".into();
    doc.charts.truncate(2);
    rig.ws.apply_state(&serde_json::to_value(&doc).unwrap());
    assert_eq!(restored.get(), 2, "a full rebuild");
    // Garbage restores nothing and raises nothing.
    rig.ws.apply_state(&json!(42));
    assert_eq!(restored.get(), 2);
}

#[test]
fn a_dangling_weak_handle_never_upgrades_and_a_live_one_does() {
    assert!(WeakWorkspace::dangling().upgrade().is_none());
    let rig = quad();
    let weak = rig.ws.downgrade();
    assert!(weak.upgrade().is_some());
    assert!(weak.clone().upgrade().is_some());
}

// ── drawing tool preferences (workspace-drawing-tools-prefs) ──

fn style_patch(color: &str, width: f64) -> crate::drawings::controller::DrawingUpdate {
    crate::drawings::controller::DrawingUpdate {
        style: Some(crate::drawings::PartialDrawingStyle {
            line_color: Some(color.to_string()),
            line_width: Some(width),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn styling_a_trend_line_on_one_chart_makes_the_next_one_on_another_chart_look_the_same() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap().try_chart().unwrap();
    let b = rig.ws.cell("b").unwrap().try_chart().unwrap();
    let id = trendline(&a, 100.0);
    a.drawings().update(&id, style_patch("#ff8800", 3.0));
    let next = trendline(&b, 100.0);
    let drawn = b
        .drawings()
        .all()
        .into_iter()
        .find(|d| d.id == next)
        .unwrap();
    assert_eq!(drawn.style.line_color, "#ff8800");
    assert_eq!(drawn.style.line_width, 3.0);
}

#[test]
fn the_magnet_and_stay_mode_come_back_after_a_restore() {
    let first = Rig::new("1", &[("a", "AAA")]);
    let c = first.ws.cell("a").unwrap().try_chart().unwrap();
    c.drawings()
        .set_snap_mode(crate::drawings::SnapMode::Strong);
    c.drawings().set_stay_mode(true);
    let saved =
        serde_json::from_str::<Value>(&crate::state::encode_state(&first.ws.get_state())).unwrap();
    assert_eq!(saved["drawingTools"]["magnet"], "strong");
    assert_eq!(saved["drawingTools"]["stay"], true);
    let second = Rig::new("1", &[("a", "AAA")]);
    second.ws.apply_state(&saved);
    let restored = second.ws.cell("a").unwrap().try_chart().unwrap();
    assert_eq!(
        restored.drawings().get_snap_mode(),
        crate::drawings::SnapMode::Strong
    );
}

#[test]
fn a_style_saved_on_one_chart_is_offered_on_every_chart_and_survives_a_restore() {
    let first = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = first.ws.cell("a").unwrap().try_chart().unwrap();
    let b = first.ws.cell("b").unwrap().try_chart().unwrap();
    let templates = json!({ "trendline": [{ "name": "Weekly", "settings": { "style": { "lineColor": "#ff8800", "lineWidth": 3 } } }] });
    a.drawings().set_tool_templates(&templates);
    let offered = b.drawings().tool_templates();
    assert_eq!(offered["trendline"].len(), 1, "offered on the other chart");
    assert_eq!(offered["trendline"][0].name, "Weekly");
    let saved =
        serde_json::from_str::<Value>(&crate::state::encode_state(&first.ws.get_state())).unwrap();
    assert_eq!(
        saved["drawingTools"]["templates"]["trendline"][0]["name"],
        "Weekly"
    );
    let second = Rig::new("1", &[("a", "AAA")]);
    second.ws.apply_state(&saved);
    let restored = second.ws.cell("a").unwrap().try_chart().unwrap();
    assert_eq!(restored.drawings().tool_templates()["trendline"].len(), 1);
}

#[test]
fn untouched_drawing_preferences_add_nothing_to_the_saved_document() {
    let rig = Rig::new("1", &[("a", "AAA")]);
    assert!(rig.ws.get_state().drawing_tools.is_none());
}

#[test]
fn a_workspace_persisted_to_storage_boots_with_the_remembered_tool_settings_in_place() {
    let storage = MemoryStorage::new();
    let first = Rig::persisted("1", &[("a", "AAA")], Rc::new(storage.clone()), "prefs");
    let c = first.ws.cell("a").unwrap().try_chart().unwrap();
    let anchor = crate::drawings::DrawingPoint {
        time: (T0 + 3_600_000) as f64,
        price: 100.0,
    };
    c.drawings().add(
        "hline",
        crate::drawings::controller::AddInit {
            anchors: Some(vec![anchor]),
            style: Some(crate::drawings::PartialDrawingStyle {
                line_color: Some("#00aa55".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    first.ws.destroy(); // flushes the pending save
    let second = Rig::persisted("1", &[("a", "AAA")], Rc::new(storage), "prefs");
    let next = second
        .ws
        .cell("a")
        .unwrap()
        .try_chart()
        .unwrap()
        .drawings()
        .add(
            "hline",
            crate::drawings::controller::AddInit {
                anchors: Some(vec![anchor]),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(next.style.line_color, "#00aa55");
}
