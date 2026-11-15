//! Layout picker: the topbar's LAYOUT dropdown. A bounded 4x4 grid canvas: hover previews
//! the full rows x cols rectangle from the top-left (the table-insert idiom); a click
//! applies it immediately. Registered layouts that are NOT expressible on the canvas
//! (bespoke plugin presets) list as labeled rows under it, and the workspace SYNC switches
//! sit beside the grid.
//!
//! This is the pure half of the original `LayoutPicker`: the grid descriptions
//! ([`grid_cells`], [`layout_grid_label`], [`paint_layout_grid`]), the hover state
//! ([`GridHover`]) and the popover placement math ([`popover_left`], [`popover_top`]).
//! Left for the browser crate: the anchored popover (outside-pointerdown and Escape dismiss,
//! enter/exit animation, surface announcements), the square, preset and switch elements and
//! CSS, the help tooltip, and the trigger's `aria-expanded`.

use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};

/// The picker canvas is a fixed 4x4: 16 cells, the workspace pool capacity.
pub const GRID: usize = 4;

/// The current layout's footprint on the canvas (mirrors the workspace's `LayoutShape`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutPickerShape {
    pub rows: usize,
    pub cols: usize,
}

/// A preset or sync row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerRow {
    pub id: String,
    pub label: String,
    pub checked: bool,
}

pub fn layout_heading() -> MessageLike {
    msg("widget.layoutPicker.layout", "Layout")
}
pub fn sync_heading() -> MessageLike {
    msg("widget.layoutPicker.sync", "Sync")
}
/// The help copy of the LAYOUT "?" badge.
pub fn grid_hint() -> MessageLike {
    msg(
        "widget.layoutPicker.gridHint",
        "Click a square to apply that columns \u{d7} rows layout.",
    )
}

/// A grid layout's name, columns by rows (`2 \u{d7} 3`), in the shell's language.
pub fn layout_grid_label(cols: usize, rows: usize, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg("widget.layoutPicker.gridSize", "{cols} \u{d7} {rows}"),
        &[
            ("cols", MessageArg::from(cols)),
            ("rows", MessageArg::from(rows)),
        ],
    )
}

/// One square of the canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridCell {
    pub row: usize,
    pub col: usize,
    /// The square's accessible name (`cols \u{d7} rows` of the layout it applies).
    pub label: String,
}

/// The bare 4x4 canvas (16 squares, row-major). Interaction wiring stays with the caller:
/// the desktop popover adds hover previews, the mobile drawer taps to apply.
pub fn grid_cells(i18n: Option<&dyn Translator>) -> Vec<GridCell> {
    (0..GRID)
        .flat_map(|row| (0..GRID).map(move |col| (row, col)))
        .map(|(row, col)| GridCell {
            row,
            col,
            label: layout_grid_label(col + 1, row + 1, i18n),
        })
        .collect()
}

/// Light the `shape` rectangle from the top-left (`None`: nothing lit). Returns the lit
/// flag of each square, row-major.
pub fn paint_layout_grid(shape: Option<LayoutPickerShape>) -> Vec<bool> {
    (0..GRID)
        .flat_map(|row| (0..GRID).map(move |col| (row, col)))
        .map(|(row, col)| shape.is_some_and(|s| row < s.rows && col < s.cols))
        .collect()
}

/// The hover preview. The preview rectangle is the hovered square's footprint; with no
/// hover the canvas shows the current layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridHover {
    hover: Option<LayoutPickerShape>,
}

impl GridHover {
    /// The pointer moved over square `(row, col)` (0-based). Returns whether the preview
    /// changed (and the canvas must repaint).
    pub fn on_square(&mut self, row: usize, col: usize) -> bool {
        let next = LayoutPickerShape {
            rows: row + 1,
            cols: col + 1,
        };
        if self.hover == Some(next) {
            return false;
        }
        self.hover = Some(next);
        true
    }

    /// The pointer left the canvas. Returns whether the preview changed.
    pub fn on_leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// The panel opened: no preview.
    pub fn reset(&mut self) {
        self.hover = None;
    }

    /// What the canvas shows: the hover preview, else the current layout's shape.
    pub fn shown(&self, current: Option<LayoutPickerShape>) -> Option<LayoutPickerShape> {
        self.hover.or(current)
    }
}

/// Where a click on square `(row, col)` applies: `(rows, cols)`.
pub fn grid_selection(row: usize, col: usize) -> (usize, usize) {
    (row + 1, col + 1)
}

/// Anchor under the trigger, clamped to the host's right edge: the panel's `left` in
/// host-local px. `trigger_left` / `host_left` are viewport coordinates.
pub fn popover_left(trigger_left: f64, host_left: f64, panel_width: f64, host_width: f64) -> f64 {
    let left = trigger_left - host_left;
    if left + panel_width > host_width - 8.0 {
        (host_width - 8.0 - panel_width).max(8.0)
    } else {
        left
    }
}

/// The panel's `top` in host-local px: under the trigger, with the kit menus' positioning
/// gutter so every topbar dropdown clears the bar alike.
pub fn popover_top(trigger_bottom: f64, host_top: f64) -> f64 {
    trigger_bottom - host_top + 8.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(shape: Option<LayoutPickerShape>) -> Vec<(usize, usize)> {
        paint_layout_grid(shape)
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| (i / GRID, i % GRID))
            .collect()
    }

    #[test]
    fn the_canvas_is_sixteen_squares_row_major_each_named_by_its_footprint() {
        let cells = grid_cells(None);
        assert_eq!(cells.len(), 16);
        assert_eq!(
            (cells[0].row, cells[0].col, cells[0].label.as_str()),
            (0, 0, "1 \u{d7} 1")
        );
        assert_eq!(
            (cells[1].row, cells[1].col, cells[1].label.as_str()),
            (0, 1, "2 \u{d7} 1")
        );
        assert_eq!(
            (cells[4].row, cells[4].col, cells[4].label.as_str()),
            (1, 0, "1 \u{d7} 2")
        );
        assert_eq!(cells[15].label, "4 \u{d7} 4");
        assert_eq!(layout_grid_label(2, 3, None), "2 \u{d7} 3");
    }

    #[test]
    fn painting_lights_the_rectangle_from_the_top_left() {
        assert!(lit(None).is_empty());
        assert_eq!(
            lit(Some(LayoutPickerShape { rows: 1, cols: 2 })),
            [(0, 0), (0, 1)]
        );
        assert_eq!(
            lit(Some(LayoutPickerShape { rows: 2, cols: 2 })),
            [(0, 0), (0, 1), (1, 0), (1, 1)]
        );
        assert_eq!(lit(Some(LayoutPickerShape { rows: 4, cols: 4 })).len(), 16);
        // A shape beyond the canvas is clipped to it; an empty one lights nothing.
        assert_eq!(lit(Some(LayoutPickerShape { rows: 9, cols: 9 })).len(), 16);
        assert!(lit(Some(LayoutPickerShape { rows: 0, cols: 3 })).is_empty());
    }

    #[test]
    fn hovering_previews_the_footprint_and_leaving_restores_the_current_layout() {
        let current = Some(LayoutPickerShape { rows: 1, cols: 1 });
        let mut hover = GridHover::default();
        assert_eq!(hover.shown(current), current);
        assert!(hover.on_square(1, 2));
        assert_eq!(
            hover.shown(current),
            Some(LayoutPickerShape { rows: 2, cols: 3 })
        );
        assert!(!hover.on_square(1, 2), "the same square repaints nothing");
        assert!(hover.on_square(0, 0));
        assert!(hover.on_leave());
        assert!(!hover.on_leave());
        assert_eq!(hover.shown(current), current);
        // A layout the canvas cannot express lights nothing until hovered.
        assert_eq!(hover.shown(None), None);
        hover.on_square(3, 3);
        hover.reset();
        assert_eq!(hover.shown(None), None);
    }

    #[test]
    fn a_click_applies_columns_by_rows() {
        assert_eq!(grid_selection(0, 0), (1, 1));
        assert_eq!(grid_selection(2, 3), (3, 4));
    }

    #[test]
    fn the_popover_anchors_under_the_trigger_clamped_to_the_host() {
        assert_eq!(popover_left(120.0, 20.0, 280.0, 800.0), 100.0);
        // Near the right edge: pulled back inside by 8px.
        assert_eq!(popover_left(700.0, 0.0, 280.0, 800.0), 512.0);
        // A panel wider than the host still keeps its 8px left gutter.
        assert_eq!(popover_left(10.0, 0.0, 900.0, 800.0), 8.0);
        assert_eq!(popover_top(60.0, 10.0), 58.0);
    }

    #[test]
    fn the_headings_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(layout_heading()), "Layout");
        assert_eq!(t(sync_heading()), "Sync");
        assert!(t(grid_hint()).starts_with("Click a square"));
    }
}
