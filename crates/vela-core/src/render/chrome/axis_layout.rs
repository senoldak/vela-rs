//! Right-gutter column geometry, shared by the renderer (which reserves the gutter and
//! routes input) and the chrome layer (which draws the tick labels). The gutter holds the
//! pane's master price scale in the leftmost column (nearest the data), and one extra
//! column per merged (own-scale) indicator to its right.

/// px: the pane master-scale column (nearest the data).
pub const AXIS_MASTER_W: f64 = 64.0;
/// px: each additional merged-indicator scale column.
pub const AXIS_MERGED_W: f64 = 56.0;

/// Thickness of the horizontal separator drawn at each stacked pane's top edge (data and
/// gutter).
pub const PANE_SEPARATOR_PX: f64 = 3.0;

/// Total right-gutter width for a given number of merged scale columns.
pub fn axis_gutter_width(merged_columns: usize) -> f64 {
    AXIS_MASTER_W + AXIS_MERGED_W * merged_columns as f64
}

/// Left x (relative to the data area's right edge `data_w`) of a scale column.
/// Column 0 is the master scale; columns 1 and up are merged-indicator scales.
pub fn axis_column_x(data_w: f64, column: usize) -> f64 {
    if column == 0 {
        data_w
    } else {
        data_w + AXIS_MASTER_W + (column - 1) as f64 * AXIS_MERGED_W
    }
}

/// Width of a scale column (master vs merged).
pub fn axis_column_width(column: usize) -> f64 {
    if column == 0 {
        AXIS_MASTER_W
    } else {
        AXIS_MERGED_W
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gutter_is_the_master_column_plus_one_per_merged_scale() {
        assert_eq!(axis_gutter_width(0), 64.0);
        assert_eq!(axis_gutter_width(1), 120.0);
        assert_eq!(axis_gutter_width(3), 232.0);
    }

    #[test]
    fn columns_stack_rightward_from_the_data_edge() {
        assert_eq!(axis_column_x(500.0, 0), 500.0);
        assert_eq!(axis_column_x(500.0, 1), 564.0);
        assert_eq!(axis_column_x(500.0, 2), 620.0);
        assert_eq!(axis_column_width(0), 64.0);
        assert_eq!(axis_column_width(1), 56.0);
        assert_eq!(axis_column_width(7), 56.0);
    }

    #[test]
    fn the_columns_tile_the_gutter() {
        let merged = 3;
        let end = axis_column_x(0.0, merged) + axis_column_width(merged);
        assert_eq!(end, axis_gutter_width(merged));
    }
}
