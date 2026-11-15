//! Ports of `workspace-screenshot.test.ts`: hidden cells drop out, adjacent tiles share an
//! edge after DPR rounding, and every contributed raster is blitted in place.

use std::rc::Rc;

use super::*;
use crate::render::canvas::RecordingCanvas;

fn grid() -> GridBox {
    GridBox {
        left: 10.0,
        top: 20.0,
        width: 200.0,
        height: 100.0,
    }
}

fn cell(left: f64, top: f64, width: f64, height: f64) -> CellBox {
    CellBox {
        hidden: false,
        left,
        top,
        width,
        height,
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> ShotRect {
    ShotRect {
        x,
        y,
        width,
        height,
    }
}

fn image(label: &str) -> CanvasImage {
    Rc::new(label.to_string())
}

fn tile(label: &str, r: ShotRect) -> LayoutShotTile {
    LayoutShotTile {
        background: None,
        sources: vec![image(label)],
        rect: r,
    }
}

#[test]
fn tiles_map_each_cell_into_grid_local_css_pixels() {
    let tiles = tiles_from_cell_rects(
        &grid(),
        &[
            cell(10.0, 20.0, 99.0, 100.0),
            cell(111.0, 20.0, 99.0, 100.0),
        ],
    );
    assert_eq!(
        tiles,
        [rect(0.0, 0.0, 99.0, 100.0), rect(101.0, 0.0, 99.0, 100.0)]
    );
}

#[test]
fn tiles_drop_hidden_cells_and_zero_size_boxes() {
    let hidden = CellBox {
        hidden: true,
        ..cell(10.0, 20.0, 99.0, 100.0)
    };
    let tiles = tiles_from_cell_rects(
        &grid(),
        &[
            hidden,
            cell(111.0, 20.0, 99.0, 100.0),
            cell(10.0, 20.0, 0.0, 100.0),
        ],
    );
    assert_eq!(tiles, [rect(101.0, 0.0, 99.0, 100.0)]);
}

#[test]
fn tiles_of_a_zero_size_grid_are_empty() {
    let flat = GridBox {
        left: 0.0,
        top: 0.0,
        width: 0.0,
        height: 100.0,
    };
    assert!(tiles_from_cell_rects(&flat, &[cell(0.0, 0.0, 50.0, 100.0)]).is_empty());
}

#[test]
fn dest_rect_makes_adjacent_tiles_share_an_edge() {
    let dpr = 1.5;
    let a = dest_rect(&rect(0.0, 0.0, 99.0, 100.0), dpr);
    let b = dest_rect(&rect(101.0, 0.0, 99.0, 100.0), dpr);
    assert_eq!(a.dx + a.dw, (99.0f64 * dpr).round());
    assert_eq!(b.dx, (101.0f64 * dpr).round());
    assert!(a.dx + a.dw <= b.dx);
}

#[test]
fn composite_fills_the_seam_color_first_then_blits_every_tile_at_its_dest_rect() {
    let canvas = RecordingCanvas::new();
    let tiles = [
        tile("a", rect(0.0, 0.0, 99.0, 100.0)),
        tile("b", rect(101.0, 0.0, 99.0, 100.0)),
    ];
    let frame = LayoutShotFrame {
        width: 200.0,
        height: 100.0,
        dpr: 2.0,
        gap_color: "rgb(40, 40, 40)".into(),
    };
    assert_eq!(frame.backing_size(), (400.0, 200.0));
    assert!(composite_layout_screenshot(&canvas, &frame, &tiles));
    let fills = canvas.calls("fillRect");
    assert_eq!(fills.len(), 1);
    assert_eq!(
        fills[0],
        serde_json::json!(["fillRect", 0.0, 0.0, 400.0, 200.0])
    );
    let draws = canvas.calls("drawImage");
    assert_eq!(draws.len(), 2);
    for (draw, t) in draws.iter().zip(&tiles) {
        let d = dest_rect(&t.rect, 2.0);
        assert_eq!(
            *draw,
            serde_json::json!(["drawImage", d.dx, d.dy, d.dw, d.dh])
        );
    }
}

#[test]
fn composite_paints_a_tile_background_and_every_source_of_its_pile() {
    let canvas = RecordingCanvas::new();
    let tiles = [LayoutShotTile {
        background: Some("#101010".into()),
        sources: vec![image("backdrop"), image("data"), image("chrome")],
        rect: rect(0.0, 0.0, 100.0, 50.0),
    }];
    let frame = LayoutShotFrame {
        width: 100.0,
        height: 50.0,
        dpr: 1.0,
        gap_color: "#000".into(),
    };
    assert!(composite_layout_screenshot(&canvas, &frame, &tiles));
    assert_eq!(
        canvas.calls("fillRect").len(),
        2,
        "the seam fill and the tile background"
    );
    assert_eq!(canvas.calls("drawImage").len(), 3);
}

#[test]
fn composite_paints_nothing_when_the_frame_is_empty_or_no_tile_paints() {
    let frame = |width: f64, height: f64| LayoutShotFrame {
        width,
        height,
        dpr: 1.0,
        gap_color: "#000".into(),
    };
    let canvas = RecordingCanvas::new();
    assert!(!composite_layout_screenshot(
        &canvas,
        &frame(0.0, 100.0),
        &[tile("a", rect(0.0, 0.0, 10.0, 10.0))]
    ));
    assert!(!composite_layout_screenshot(
        &canvas,
        &frame(200.0, 100.0),
        &[]
    ));
    assert!(!composite_layout_screenshot(
        &canvas,
        &frame(200.0, 100.0),
        &[tile("a", rect(0.0, 0.0, 0.0, 10.0))]
    ));
    assert!(canvas.ops().is_empty() || canvas.calls("drawImage").is_empty());
}

#[test]
fn a_bad_dpr_reads_as_one() {
    let frame = LayoutShotFrame {
        width: 200.0,
        height: 100.0,
        dpr: 0.0,
        gap_color: "#000".into(),
    };
    assert_eq!(frame.backing_size(), (200.0, 100.0));
    let tiny = LayoutShotFrame {
        width: 0.1,
        height: 0.1,
        dpr: 1.0,
        gap_color: "#000".into(),
    };
    assert_eq!(tiny.backing_size(), (1.0, 1.0), "never below one pixel");
}

#[test]
fn the_overlay_hook_runs_under_before_the_canvases_and_over_after_them() {
    use std::cell::RefCell;

    let canvas = RecordingCanvas::new();
    let frame = LayoutShotFrame {
        width: 200.0,
        height: 100.0,
        dpr: 1.0,
        gap_color: "#000".into(),
    };
    let tiles = [
        tile("a", rect(0.0, 0.0, 99.0, 100.0)),
        tile("b", rect(101.0, 0.0, 99.0, 100.0)),
    ];
    let seen = RefCell::new(Vec::new());
    let painted = composite_layout_screenshot_with(&canvas, &frame, &tiles, &|i, phase, d| {
        seen.borrow_mut().push((i, phase, d.dx));
    });
    assert!(painted);
    assert_eq!(
        *seen.borrow(),
        [
            (0, OverlayPhase::Under, 0.0),
            (0, OverlayPhase::Over, 0.0),
            (1, OverlayPhase::Under, 101.0),
            (1, OverlayPhase::Over, 101.0),
        ]
    );
}
