//! Ports of the splitter and seam sections of `workspace-core.test.ts`.

use super::*;
use crate::workspace::layouts::{LayoutCell, LayoutDefinition, LayoutRegistry, occupancy_grid};

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} is not {b}");
}

fn grid(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect()
}

fn stacked32() -> LayoutDefinition {
    LayoutDefinition {
        id: "stacked-3-2".into(),
        label: "Stacked 3-2".into(),
        cols: vec![1.0, 1.0],
        rows: vec![1.0; 6],
        areas: Some(
            ["c1 c4", "c1 c4", "c2 c4", "c2 c5", "c3 c5", "c3 c5"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        cells: (1..=5)
            .map(|i| LayoutCell::in_area(format!("c{i}"), format!("c{i}")))
            .collect(),
    }
}

#[test]
fn even_tracks_resets_to_a_uniform_split() {
    assert_eq!(even_tracks(3), [1.0, 1.0, 1.0]);
    assert!(even_tracks(0).is_empty());
}

#[test]
fn resize_tracks_trades_weight_between_the_two_neighbors() {
    let next = resize_tracks(&[1.0, 1.0], 0, 200.0, 800.0); // +25% of the content to track 0
    close(next[0] + next[1], 2.0);
    close(next[0], 1.5);
    close(next[1], 0.5);
    let four = resize_tracks(&[1.0; 4], 1, 100.0, 400.0);
    assert_eq!(four[0], 1.0);
    assert_eq!(four[3], 1.0);
    close(four[1] + four[2], 2.0);
}

#[test]
fn resize_tracks_clamps_so_neither_neighbor_drops_under_ten_percent() {
    let shrunk = resize_tracks(&[1.0, 1.0], 0, -10_000.0, 800.0);
    close(shrunk[0], 0.2); // 10% of total (2)
    close(shrunk[1], 1.8);
    let grown = resize_tracks(&[1.0, 1.0], 0, 10_000.0, 800.0);
    close(grown[1], 0.2);
}

#[test]
fn resize_tracks_ignores_bad_input() {
    let w = [1.0, 1.0];
    assert_eq!(resize_tracks(&w, 1, 50.0, 800.0), w); // no track after the last
    assert_eq!(resize_tracks(&w, 9, 50.0, 800.0), w);
    assert_eq!(resize_tracks(&w, 0, 50.0, 0.0), w);
    assert_eq!(resize_tracks(&w, 0, 50.0, -5.0), w);
    assert!(resize_tracks(&[], 0, 50.0, 800.0).is_empty());
}

#[test]
fn track_offsets_returns_internal_boundary_centers() {
    assert_eq!(track_offsets(&[1.0, 1.0], 800.0, 4.0), [400.0]);
    assert_eq!(track_offsets(&[1.0; 4], 800.0, 0.0), [200.0, 400.0, 600.0]);
    assert!(track_offsets(&[1.0], 800.0, 4.0).is_empty());
    assert!(track_offsets(&[], 800.0, 4.0).is_empty());
    assert!(track_offsets(&[0.0, 0.0], 800.0, 4.0).is_empty());
}

#[test]
fn a_uniform_grid_boundary_is_one_full_length_seam() {
    let reg = LayoutRegistry::with_builtins();
    let g = occupancy_grid(&reg.definition("4", None).unwrap());
    assert_eq!(seam_segments(&g, Axis::Cols, 0), [(0, 1)]);
    assert_eq!(seam_segments(&g, Axis::Rows, 0), [(0, 1)]);
}

#[test]
fn a_row_boundary_inside_one_stack_never_covers_the_neighboring_spanning_cell() {
    let g = occupancy_grid(&stacked32());
    assert_eq!(g.len(), 6);
    assert_eq!(g[0], ["c1", "c4"]);
    assert_eq!(g[2], ["c2", "c4"]);
    assert_eq!(g[5], ["c3", "c5"]);
    assert_eq!(seam_segments(&g, Axis::Rows, 1), [(0, 0)]);
    assert_eq!(seam_segments(&g, Axis::Rows, 2), [(1, 1)]);
    assert!(seam_segments(&g, Axis::Rows, 0).is_empty());
    assert_eq!(seam_segments(&g, Axis::Cols, 0), [(0, 5)]);
}

#[test]
fn a_spanning_tall_cell_splits_a_boundary_into_disjoint_segments() {
    let g = grid(&[&["a", "b"], &["c", "b"], &["d", "e"]]);
    assert_eq!(seam_segments(&g, Axis::Rows, 0), [(0, 0)]);
    assert_eq!(seam_segments(&g, Axis::Rows, 1), [(0, 1)]);
    assert_eq!(seam_segments(&g, Axis::Cols, 0), [(0, 2)]);
    // Two seams with a gap between them.
    let g = grid(&[&["a", "b"], &["c", "c"], &["d", "e"]]);
    assert_eq!(seam_segments(&g, Axis::Cols, 0), [(0, 0), (2, 2)]);
}

#[test]
fn seam_segments_tolerate_an_empty_or_out_of_range_grid() {
    assert!(seam_segments(&[], Axis::Cols, 0).is_empty());
    assert!(seam_segments(&[], Axis::Rows, 0).is_empty());
    // Past the last boundary both sides are absent, so they compare equal: no seam.
    let g = grid(&[&["a", "b"]]);
    assert!(seam_segments(&g, Axis::Cols, 5).is_empty());
}

#[test]
fn segment_span_runs_flush_at_container_edges_and_half_a_gap_into_interior_gaps() {
    let w = [1.0; 6];
    let full = segment_span_px(&w, 602.0, 2.0, 0, 5);
    assert_eq!(full, (0.0, 602.0));
    let first = segment_span_px(&w, 602.0, 2.0, 0, 0);
    assert_eq!(first.0, 0.0);
    assert!((first.1 - (98.666 + 1.0)).abs() < 0.1);
    let inner = segment_span_px(&w, 602.0, 2.0, 2, 3);
    assert!((inner.0 - (2.0 * (98.666 + 2.0) - 1.0)).abs() < 0.1);
    assert!((inner.1 - (4.0 * (98.666 + 2.0) - 1.0)).abs() < 0.1);
    assert_eq!(segment_span_px(&[], 100.0, 2.0, 0, 0), (0.0, 0.0));
}

#[test]
fn strips_cover_the_real_seams_of_a_2x2_grid() {
    let reg = LayoutRegistry::with_builtins();
    let g = occupancy_grid(&reg.definition("4", None).unwrap());
    let strips = splitter_strips(&[1.0, 1.0], &[1.0, 1.0], &g, 800.0, 400.0, 2.0);
    assert_eq!(strips.len(), 2);
    let col = &strips[0];
    assert_eq!(col.axis, Axis::Cols);
    assert_eq!(
        (col.left, col.top, col.width, col.height),
        (396.0, 0.0, 8.0, 400.0)
    );
    let row = &strips[1];
    assert_eq!(row.axis, Axis::Rows);
    assert_eq!(
        (row.left, row.top, row.width, row.height),
        (0.0, 196.0, 800.0, 8.0)
    );
}

#[test]
fn strips_of_an_area_layout_stop_at_spanning_cells() {
    let g = occupancy_grid(&stacked32());
    let strips = splitter_strips(&[1.0, 1.0], &[1.0; 6], &g, 600.0, 600.0, 0.0);
    let rows: Vec<&SplitterStrip> = strips.iter().filter(|s| s.axis == Axis::Rows).collect();
    // Boundaries 1 and 3 split the left column only; boundary 2 the right column only.
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].index, 1);
    assert!(rows[0].left == 0.0 && rows[0].width == 300.0);
    assert_eq!(rows[1].index, 2);
    assert!(rows[1].left == 300.0 && rows[1].width == 300.0);
    assert_eq!(rows[2].index, 3);
    assert!(rows[2].left == 0.0 && rows[2].width == 300.0);
}

#[test]
fn a_drag_trades_weight_from_its_starting_weights() {
    let drag = SplitterDrag::begin(Axis::Cols, 0, 400.0, &[1.0, 1.0], 802.0, 2.0);
    assert_eq!(drag.axis(), Axis::Cols);
    let after = drag.update(600.0); // 200px of 800px of content
    close(after[0], 1.5);
    close(after[1], 0.5);
    // The drag always works from where it started: moving back restores.
    assert_eq!(drag.update(400.0), [1.0, 1.0]);
}
