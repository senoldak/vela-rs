//! The planning half of the table overlay: what a Pine `table.new` object renders, with
//! no DOM. The overlay itself (a positioned container appended to the chart element, one
//! per indicator, rebuilt from the table snapshot each tick) and its canvas painter are
//! the later stages' and the browser crate's.

use std::collections::{HashMap, HashSet};

use crate::model::drawings::{BoxTextSize, DrawingTable, TableTextSize};

/// Font px of the `auto` size.
const AUTO_PX: f64 = 13.0;

fn named_px(size: BoxTextSize) -> f64 {
    match size {
        BoxTextSize::Auto => AUTO_PX,
        BoxTextSize::Tiny => 10.0,
        BoxTextSize::Small => 11.0,
        BoxTextSize::Normal => 13.0,
        BoxTextSize::Large => 16.0,
        BoxTextSize::Huge => 20.0,
    }
}

/// Font size in px: a Pine integer `text_size` passes through as-is (0 or less reads as
/// auto).
pub fn font_px_of(size: TableTextSize) -> f64 {
    match size {
        TableTextSize::Px(px) => {
            if px > 0.0 {
                px
            } else {
                AUTO_PX
            }
        }
        TableTextSize::Named(named) => named_px(named),
    }
}

/// True when at least one cell was actually set via `table.cell()`. A merely allocated
/// table (`table.new` with rows x columns of nulls, or only merge-absorbed stubs)
/// occupies no space in Pine: it must not paint, not even its background or frame.
pub fn table_has_content(t: &DrawingTable) -> bool {
    t.cells
        .iter()
        .any(|row| row.iter().any(|c| c.as_ref().is_some_and(|c| !c.merged)))
}

/// A merge region's origin spans, in columns and rows (a malformed region may be zero or
/// negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeSpan {
    pub cs: i64,
    pub rs: i64,
}

/// How each merge region renders: the origin spans, the absorbed cells are omitted. Cells
/// are keyed `(row, col)` (the original keys them `"row:col"`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeRenderPlan {
    /// Origin cell to its column and row spans.
    pub span: HashMap<(usize, usize), MergeSpan>,
    /// Cells to omit entirely (absorbed by a merge region).
    pub omit: HashSet<(usize, usize)>,
}

/// Resolve merge regions to spans and omissions. Defensive on two engine quirks:
/// duplicate merge records are idempotent, and a `merged` flag stamped on the ORIGIN cell
/// never omits it. Only genuinely absorbed cells are omitted (region members other than
/// the origin, plus stray `merged`-flagged cells whose region record is missing).
///
/// A region reaching past the table's rows or columns omits only the cells inside the
/// table: nothing outside it is ever painted or asked about.
pub fn merge_render_plan(t: &DrawingTable) -> MergeRenderPlan {
    let mut span = HashMap::new();
    let mut omit = HashSet::new();
    for m in &t.merges {
        span.insert(
            (m.start_row, m.start_col),
            MergeSpan {
                cs: m.end_col as i64 - m.start_col as i64 + 1,
                rs: m.end_row as i64 - m.start_row as i64 + 1,
            },
        );
        let last_row = m.end_row.min(t.rows.saturating_sub(1));
        let last_col = m.end_col.min(t.columns.saturating_sub(1));
        for r in m.start_row..=last_row {
            for c in m.start_col..=last_col {
                if r != m.start_row || c != m.start_col {
                    omit.insert((r, c));
                }
            }
        }
    }
    for r in 0..t.rows {
        for c in 0..t.columns {
            let merged = t
                .cells
                .get(r)
                .and_then(|row| row.get(c))
                .and_then(Option::as_ref)
                .is_some_and(|cell| cell.merged);
            if merged && !span.contains_key(&(r, c)) {
                omit.insert((r, c));
            }
        }
    }
    for key in span.keys() {
        omit.remove(key);
    }
    MergeRenderPlan { span, omit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::drawings::{
        BoxFontFamily, BoxHAlign, BoxVAlign, TableCell, TableMerge, TablePosition,
    };

    fn cell(text: Option<&str>, merged: bool) -> Option<TableCell> {
        Some(TableCell {
            text: text.map(String::from),
            text_color: None,
            bg_color: None,
            h_align: BoxHAlign::Center,
            v_align: BoxVAlign::Center,
            text_size: TableTextSize::Named(BoxTextSize::Normal),
            font_family: BoxFontFamily::Default,
            tooltip: None,
            bold: false,
            italic: false,
            width: None,
            height: None,
            merged,
        })
    }

    fn plain() -> Option<TableCell> {
        cell(None, false)
    }

    fn table_of(cells: Vec<Vec<Option<TableCell>>>, merges: Vec<TableMerge>) -> DrawingTable {
        DrawingTable {
            id: "t".into(),
            pane_id: "price".into(),
            position: TablePosition::TopRight,
            columns: cells.first().map_or(0, Vec::len),
            rows: cells.len(),
            bg_color: None,
            frame_color: None,
            frame_width: 0.0,
            border_color: None,
            border_width: 0.0,
            cells,
            merges,
            overlay: false,
        }
    }

    fn merge(start_row: usize, start_col: usize, end_row: usize, end_col: usize) -> TableMerge {
        TableMerge {
            start_row,
            start_col,
            end_row,
            end_col,
        }
    }

    // TableOverlay: fontPxOf

    #[test]
    fn an_integer_pine_text_size_passes_through_as_raw_pixels() {
        assert_eq!(font_px_of(TableTextSize::Px(14.0)), 14.0);
        assert_eq!(font_px_of(TableTextSize::Px(23.0)), 23.0);
    }

    #[test]
    fn named_sizes_map_and_zero_or_auto_is_the_auto_size() {
        assert_eq!(font_px_of(TableTextSize::Named(BoxTextSize::Large)), 16.0);
        assert_eq!(font_px_of(TableTextSize::Named(BoxTextSize::Auto)), 13.0);
        assert_eq!(font_px_of(TableTextSize::Px(0.0)), 13.0);
        assert_eq!(font_px_of(TableTextSize::Px(-4.0)), 13.0);
        assert_eq!(font_px_of(TableTextSize::Px(f64::NAN)), 13.0);
        assert_eq!(font_px_of(TableTextSize::Named(BoxTextSize::Tiny)), 10.0);
        assert_eq!(font_px_of(TableTextSize::Named(BoxTextSize::Small)), 11.0);
        assert_eq!(font_px_of(TableTextSize::Named(BoxTextSize::Huge)), 20.0);
    }

    // TableOverlay: tableHasContent

    #[test]
    fn an_allocated_but_never_filled_table_has_no_content() {
        assert!(!table_has_content(&table_of(
            vec![vec![None, None], vec![None, None]],
            vec![]
        )));
        assert!(!table_has_content(&table_of(vec![], vec![])));
    }

    #[test]
    fn a_single_set_cell_makes_the_table_render() {
        assert!(table_has_content(&table_of(
            vec![vec![None, cell(Some("x"), false)], vec![None, None]],
            vec![]
        )));
        // A set cell with no text (bgcolor-only) still counts.
        let mut bg = plain().unwrap();
        bg.bg_color = Some("#ff0000".into());
        assert!(table_has_content(&table_of(vec![vec![Some(bg)]], vec![])));
    }

    #[test]
    fn merge_absorbed_stubs_alone_do_not_count_as_content() {
        assert!(!table_has_content(&table_of(
            vec![vec![cell(None, true), cell(None, true)]],
            vec![]
        )));
    }

    // TableOverlay: mergeRenderPlan

    #[test]
    fn the_origin_spans_and_absorbed_cells_are_omitted() {
        let t = table_of(
            vec![vec![cell(Some("T"), false), plain(), plain()]],
            vec![merge(0, 0, 0, 2)],
        );
        let plan = merge_render_plan(&t);
        assert_eq!(plan.span.get(&(0, 0)), Some(&MergeSpan { cs: 3, rs: 1 }));
        assert!(!plan.omit.contains(&(0, 0)));
        assert!(plan.omit.contains(&(0, 1)));
        assert!(plan.omit.contains(&(0, 2)));
    }

    #[test]
    fn the_origin_is_never_omitted_even_when_the_engine_stamped_merged_on_it() {
        // Repeated `table.merge_cells` in PineTS marks the ORIGIN cell `_merged`
        // (self-parent forwarding): the merged title row must still paint.
        let t = table_of(
            vec![vec![
                cell(Some("T"), true),
                cell(None, true),
                cell(None, true),
            ]],
            vec![merge(0, 0, 0, 2)],
        );
        let plan = merge_render_plan(&t);
        assert!(!plan.omit.contains(&(0, 0)));
        assert!(plan.omit.contains(&(0, 1)));
    }

    #[test]
    fn duplicate_merge_records_are_idempotent() {
        let m = merge(0, 0, 1, 1);
        let t = table_of(
            vec![
                vec![cell(Some("T"), false), plain()],
                vec![plain(), plain()],
            ],
            vec![m, m, m],
        );
        let plan = merge_render_plan(&t);
        assert_eq!(plan.span.len(), 1);
        assert_eq!(plan.omit, HashSet::from([(0, 1), (1, 0), (1, 1)]));
    }

    #[test]
    fn a_stray_absorbed_stub_without_its_merge_record_is_still_omitted() {
        let t = table_of(vec![vec![cell(Some("a"), false), cell(None, true)]], vec![]);
        let plan = merge_render_plan(&t);
        assert!(plan.omit.contains(&(0, 1)));
        assert!(!plan.omit.contains(&(0, 0)));
    }

    #[test]
    fn a_region_reaching_past_the_table_omits_only_the_cells_inside_it() {
        let t = table_of(
            vec![vec![cell(Some("T"), false), plain()]],
            vec![merge(0, 0, 1_000_000, 1_000_000)],
        );
        let plan = merge_render_plan(&t);
        assert_eq!(plan.omit, HashSet::from([(0, 1)]));
        assert_eq!(plan.span.get(&(0, 0)).map(|s| s.cs), Some(1_000_001));
    }

    #[test]
    fn a_reversed_region_spans_nothing_and_omits_nothing() {
        let t = table_of(vec![vec![plain(), plain()]], vec![merge(0, 1, 0, 0)]);
        let plan = merge_render_plan(&t);
        assert!(plan.omit.is_empty());
        assert_eq!(plan.span.get(&(0, 1)), Some(&MergeSpan { cs: 0, rs: 1 }));
    }
}
