//! The workspace LAYOUT engine: pure data and pure functions (`layouts.ts`).
//!
//! A layout is a registered descriptor (the same idiom as the chart-type registry): a
//! grid of weighted tracks plus the SLOTS it exposes. Slot ids are canonical (`c1`..`cN`)
//! and shared by every layout, which is why two layouts need no id coordination, but a
//! slot id is pure geometry, NOT a cell identity: which cell lives in slot i is the
//! workspace's call (its `cells` declaration order), and a cell dropped by a smaller
//! layout parks its state in the pool under its own identity. Asymmetric layouts map
//! slots onto named grid areas; the slot ids stay `cN` regardless.
//!
//! [`LayoutRegistry::register`] is the SDK seam: plugins and hosts add denser grids or
//! bespoke arrangements, and every layout picker reads the registry live.
//!
//! # Differences from the original
//!
//! - The registry is an explicit value ([`LayoutRegistry`], a shared handle: clones see
//!   the same layouts) the host passes to the workspace, not a module global.
//! - `gridStyles` returns [`GridStyles`], the same facts as typed data (the CSS strings
//!   the browser writes are produced by [`GridStyles::template_columns`] and friends).
//! - `layoutGlyph` returns the complete `<svg>` markup, as `svg16` wraps it in the kit.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::i18n::{MessageArg, MessageLike, Translator, msg, source_text, text_of, with_values};
use crate::state::TrackSizes;

/// One slot of a layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutCell {
    /// The slot id (`c1`..`cN` for the built-ins).
    pub id: String,
    /// The named grid area the slot binds to (asymmetric layouts only).
    pub area: Option<String>,
}

impl LayoutCell {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            area: None,
        }
    }

    pub fn in_area(id: impl Into<String>, area: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            area: Some(area.into()),
        }
    }
}

/// One registered workspace layout.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDefinition {
    /// Stable id (`"4"`, `"2h"`, a plugin's `"6-tall"`, ...). Re-registering replaces.
    pub id: String,
    /// Human-readable label for layout pickers, in the language it was read in.
    pub label: String,
    /// Column track weights (`fr` units), left to right.
    pub cols: Vec<f64>,
    /// Row track weights, top to bottom.
    pub rows: Vec<f64>,
    /// Optional `grid-template-areas` rows for ASYMMETRIC layouts: one string per row,
    /// one area name per column (`["main a", "main b"]` is a tall left cell beside two
    /// stacked ones). Omitted: cells auto-flow row-major in `cells` order.
    pub areas: Option<Vec<String>>,
    /// The layout's SLOTS, in order: pure geometry. `id` keys the slot's grid styles and
    /// `area` binds it to a named area. A slot id is NOT a cell identity.
    pub cells: Vec<LayoutCell>,
}

/// What [`LayoutRegistry::register`] takes: a definition whose label may be a message,
/// read in each picker's language.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutRegistration {
    pub id: String,
    pub label: MessageLike,
    pub cols: Vec<f64>,
    pub rows: Vec<f64>,
    pub areas: Option<Vec<String>>,
    pub cells: Vec<LayoutCell>,
}

impl From<LayoutDefinition> for LayoutRegistration {
    fn from(def: LayoutDefinition) -> Self {
        Self {
            id: def.id,
            label: MessageLike::Text(def.label.into()),
            cols: def.cols,
            rows: def.rows,
            areas: def.areas,
            cells: def.cells,
        }
    }
}

/// Grid track sizes overriding a layout's declared weights (splitter drags): the shared
/// state document's [`TrackSizes`].
pub type LayoutTrackSizes = TrackSizes;

#[derive(Default)]
struct Inner {
    /// Registration order.
    order: Vec<String>,
    /// Registered layouts, labelled in English.
    defs: BTreeMap<String, LayoutDefinition>,
    /// Each registered layout's label as registered: a message reads per language.
    labels: BTreeMap<String, MessageLike>,
}

/// The registered layouts. A shared handle: clones see the same registry.
#[derive(Clone, Default)]
pub struct LayoutRegistry {
    inner: Rc<RefCell<Inner>>,
}

/// Canonical slot ids `c1`..`cN`.
fn slots(n: usize) -> Vec<LayoutCell> {
    (1..=n).map(|i| LayoutCell::new(format!("c{i}"))).collect()
}

/// Classic preset ids by uniform geometry, so grid picks land on the builtins.
fn grid_builtin_id(rows: usize, cols: usize) -> Option<&'static str> {
    match (rows, cols) {
        (1, 1) => Some("1"),
        (1, 2) => Some("2h"),
        (2, 1) => Some("2v"),
        (2, 2) => Some("4"),
        (2, 4) => Some("8"),
        _ => None,
    }
}

/// Picker canvas bound: dynamic layouts stay within a 4x4 grid (16 cells, the
/// workspace's dormant-state pool capacity).
pub const GRID_PICKER_MAX: usize = 4;

fn grid_label(rows: usize, cols: usize) -> MessageLike {
    with_values(
        &msg("workspace.layout.grid", "{cols} \u{d7} {rows} grid"),
        [
            ("cols", MessageArg::from(cols)),
            ("rows", MessageArg::from(rows)),
        ],
    )
}

fn clamp_track(n: f64) -> usize {
    // `Math.max(1, Math.min(4, Math.round(n)))`, NaN reads as 1 in a `usize`.
    let r = crate::indicators::classics::math::js_round(n);
    if r.is_nan() {
        return 1;
    }
    r.clamp(1.0, GRID_PICKER_MAX as f64) as usize
}

/// `^g([1-4])x([1-4])$`.
fn parse_grid_id(id: &str) -> Option<(usize, usize)> {
    let rest = id.strip_prefix('g')?;
    let (r, c) = rest.split_once('x')?;
    let digit = |s: &str| -> Option<usize> {
        let mut chars = s.chars();
        let d = chars.next()?.to_digit(10)? as usize;
        (chars.next().is_none() && (1..=GRID_PICKER_MAX).contains(&d)).then_some(d)
    };
    Some((digit(r)?, digit(c)?))
}

impl LayoutRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry holding the built-in presets (the original's
    /// `registerBuiltinLayouts`).
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        registry.register_builtins();
        registry
    }

    /// Register (or replace) a workspace layout. Pickers read the registry live.
    pub fn register(&self, def: impl Into<LayoutRegistration>) {
        let def = def.into();
        let mut inner = self.inner.borrow_mut();
        let label = source_text(&def.label);
        let stored = LayoutDefinition {
            id: def.id.clone(),
            label,
            cols: def.cols,
            rows: def.rows,
            areas: def.areas,
            cells: def.cells,
        };
        if !inner.defs.contains_key(&def.id) {
            inner.order.push(def.id.clone());
        }
        inner.defs.insert(def.id.clone(), stored);
        inner.labels.insert(def.id, def.label);
    }

    /// Remove a layout (unknown ids do nothing).
    pub fn unregister(&self, id: &str) {
        let mut inner = self.inner.borrow_mut();
        inner.defs.remove(id);
        inner.labels.remove(id);
        inner.order.retain(|k| k != id);
    }

    /// Register the built-in presets (idempotent).
    pub fn register_builtins(&self) {
        let one = |id: &str, label: MessageLike, cols: &[f64], rows: &[f64], n: usize| {
            self.register(LayoutRegistration {
                id: id.to_string(),
                label,
                cols: cols.to_vec(),
                rows: rows.to_vec(),
                areas: None,
                cells: slots(n),
            });
        };
        one(
            "1",
            msg("workspace.layout.single", "Single"),
            &[1.0],
            &[1.0],
            1,
        );
        one(
            "2h",
            msg("workspace.layout.sideBySide", "2 side by side"),
            &[1.0, 1.0],
            &[1.0],
            2,
        );
        one(
            "2v",
            msg("workspace.layout.stacked", "2 stacked"),
            &[1.0],
            &[1.0, 1.0],
            2,
        );
        one(
            "4",
            msg("workspace.layout.grid4", "4 grid"),
            &[1.0, 1.0],
            &[1.0, 1.0],
            4,
        );
        one(
            "8",
            msg("workspace.layout.grid8", "8 grid"),
            &[1.0, 1.0, 1.0, 1.0],
            &[1.0, 1.0],
            8,
        );
    }

    /// `def` labelled in `i18n`'s language (English without one).
    fn in_language(
        &self,
        def: &LayoutDefinition,
        i18n: Option<&dyn Translator>,
    ) -> LayoutDefinition {
        let message = self
            .inner
            .borrow()
            .labels
            .get(&def.id)
            .cloned()
            .or_else(|| parse_grid_id(&def.id).map(|(r, c)| grid_label(r, c)))
            .unwrap_or_else(|| MessageLike::Text(def.label.clone().into()));
        let label = text_of(i18n, &message, &[]);
        if label == def.label {
            def.clone()
        } else {
            LayoutDefinition {
                label,
                ..def.clone()
            }
        }
    }

    /// The definition behind a layout id (`None` for unknown ids), labelled in `i18n`'s
    /// language (English without one). Read it again after `locale:changed`.
    pub fn definition(&self, id: &str, i18n: Option<&dyn Translator>) -> Option<LayoutDefinition> {
        let def = self.inner.borrow().defs.get(id).cloned()?;
        Some(self.in_language(&def, i18n))
    }

    /// Every registered layout (registration order), labelled in `i18n`'s language
    /// (English without one): what layout pickers list.
    pub fn layouts(&self, i18n: Option<&dyn Translator>) -> Vec<LayoutDefinition> {
        let defs: Vec<LayoutDefinition> = {
            let inner = self.inner.borrow();
            inner
                .order
                .iter()
                .filter_map(|id| inner.defs.get(id).cloned())
                .collect()
        };
        defs.iter().map(|d| self.in_language(d, i18n)).collect()
    }

    /// The layout for a UNIFORM rows x cols grid (grid picker mode), clamped to the
    /// picker canvas. Geometry matching a registered classic preset returns that preset
    /// (id `"4"`, `"2h"`, ...); anything else synthesizes a `g<rows>x<cols>` definition.
    pub fn layout_for_grid(
        &self,
        rows: f64,
        cols: f64,
        i18n: Option<&dyn Translator>,
    ) -> LayoutDefinition {
        let r = clamp_track(rows);
        let c = clamp_track(cols);
        if let Some(id) = grid_builtin_id(r, c) {
            let registered = self.inner.borrow().defs.get(id).cloned();
            if let Some(def) = registered {
                return self.in_language(&def, i18n);
            }
        }
        LayoutDefinition {
            id: format!("g{r}x{c}"),
            label: text_of(i18n, &grid_label(r, c), &[]),
            cols: vec![1.0; c],
            rows: vec![1.0; r],
            areas: None,
            cells: slots(r * c),
        }
    }

    /// Resolve a layout id to its definition, synthesizing the picker's dynamic ids
    /// (`g<rows>x<cols>`) when they are not registered: the boot path for persisted
    /// picks. Unknown ids stay `None`.
    pub fn ensure(&self, id: &str) -> Option<LayoutDefinition> {
        if let Some(def) = self.inner.borrow().defs.get(id) {
            return Some(def.clone());
        }
        let (r, c) = parse_grid_id(id)?;
        Some(self.layout_for_grid(r as f64, c as f64, None))
    }
}

/// A layout's shape on the picker canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutShape {
    pub rows: usize,
    pub cols: usize,
}

/// The picker-canvas shape of a layout: rows and cols for uniform grids, `None` when the
/// layout is not expressible on the canvas (bespoke plugin presets): pickers list those
/// as labeled rows instead.
pub fn layout_shape(def: &LayoutDefinition) -> Option<LayoutShape> {
    if def.areas.is_none()
        && def.rows.len() <= GRID_PICKER_MAX
        && def.cols.len() <= GRID_PICKER_MAX
        && def.cells.len() == def.rows.len() * def.cols.len()
    {
        Some(LayoutShape {
            rows: def.rows.len(),
            cols: def.cols.len(),
        })
    } else {
        None
    }
}

/// Which slot occupies each `[row][col]` track. Area layouts read their
/// `grid-template-areas` rows; auto-flow layouts place `cells` row-major. The grid is
/// what the splitter layer segments its strips against: a boundary is only a real seam
/// where the two neighboring tracks hold DIFFERENT slots.
pub fn occupancy_grid(def: &LayoutDefinition) -> Vec<Vec<String>> {
    if let Some(areas) = &def.areas {
        return areas
            .iter()
            .map(|row| row.split_whitespace().map(str::to_string).collect())
            .collect();
    }
    let cols = def.cols.len();
    (0..def.rows.len())
        .map(|r| {
            (0..cols)
                .map(|c| {
                    def.cells
                        .get(r * cols + c)
                        .map_or_else(|| format!("\u{b7}{r}x{c}"), |cell| cell.id.clone())
                })
                .collect()
        })
        .collect()
}

/// The glyph's frame: a 13px square inset 1.5px in the 16px box.
const GLYPH_MIN: f64 = 1.5;
const GLYPH_SPAN: f64 = 13.0;

/// A number as `String(n)` writes it for the glyph's values (`8`, `4.75`, `1.5`).
fn num(n: f64) -> String {
    crate::util::js_number(n)
}

/// Track boundaries on the glyph frame, proportional to the declared weights.
fn glyph_stops(weights: &[f64], n: usize) -> Vec<f64> {
    let w: Vec<f64> = (0..n)
        .map(|i| match weights.get(i) {
            Some(v) if v.is_finite() && *v > 0.0 => *v,
            _ => 1.0,
        })
        .collect();
    let total: f64 = w.iter().sum();
    let mut stops = vec![GLYPH_MIN];
    let mut acc = 0.0;
    for v in w {
        acc += v;
        let at = GLYPH_MIN + (GLYPH_SPAN * acc) / total;
        stops.push(crate::indicators::classics::math::js_round(at * 100.0) / 100.0);
    }
    stops
}

/// The wrapper the kit's `svg16` puts around a 16px glyph body.
pub fn svg16(body: &str) -> String {
    format!(
        "<svg viewBox=\"0 0 16 16\" width=\"1em\" height=\"1em\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{body}</svg>"
    )
}

/// A 16px line glyph of a layout's cell arrangement (`<svg>` markup). The frame is the
/// whole grid; a seam is drawn only where two neighboring tracks hold DIFFERENT slots
/// (the [`occupancy_grid`] rule the splitters follow), so a cell spanning several tracks
/// reads as one. Seams sit at the declared track weights.
#[allow(clippy::needless_range_loop)] // the loops index two parallel stop tables by the track
pub fn layout_glyph(def: &LayoutDefinition) -> String {
    let grid = occupancy_grid(def);
    let rows = grid.len();
    let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
    let xs = glyph_stops(&def.cols, cols);
    let ys = glyph_stops(&def.rows, rows);
    let at = |r: usize, c: usize| grid.get(r).and_then(|row| row.get(c));
    let mut seams = String::new();
    for c in 1..cols {
        let mut start: Option<usize> = None;
        for r in 0..=rows {
            let split = r < rows && at(r, c - 1) != at(r, c);
            match (split, start) {
                (true, None) => start = Some(r),
                (false, Some(s)) => {
                    seams.push_str(&format!("M{} {}V{}", num(xs[c]), num(ys[s]), num(ys[r])));
                    start = None;
                }
                _ => {}
            }
        }
    }
    for r in 1..rows {
        let mut start: Option<usize> = None;
        for c in 0..=cols {
            let split = c < cols && at(r - 1, c) != at(r, c);
            match (split, start) {
                (true, None) => start = Some(c),
                (false, Some(s)) => {
                    seams.push_str(&format!("M{} {}H{}", num(xs[s]), num(ys[r]), num(xs[c])));
                    start = None;
                }
                _ => {}
            }
        }
    }
    let frame = format!(
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"1.5\"/>",
        num(GLYPH_MIN),
        num(GLYPH_MIN),
        num(GLYPH_SPAN),
        num(GLYPH_SPAN)
    );
    if seams.is_empty() {
        svg16(&frame)
    } else {
        svg16(&format!("{frame}<path d=\"{seams}\"/>"))
    }
}

/// The grid container's template, as the layout and any splitter override resolve it.
#[derive(Clone, Debug, PartialEq)]
pub struct GridStyles {
    /// Column track weights actually in force (`fr`).
    pub cols: Vec<f64>,
    /// Row track weights actually in force (`fr`).
    pub rows: Vec<f64>,
    /// `grid-template-areas` rows, for asymmetric layouts.
    pub areas: Option<Vec<String>>,
    /// Per slot id: the `grid-area` the cell takes (`None`: auto-flow).
    pub per_cell: BTreeMap<String, Option<String>>,
}

impl GridStyles {
    /// `grid-template-columns` (`"1fr 1fr"`).
    pub fn template_columns(&self) -> String {
        fr_list(&self.cols)
    }

    /// `grid-template-rows`.
    pub fn template_rows(&self) -> String {
        fr_list(&self.rows)
    }

    /// `grid-template-areas` (`"\"main a\" \"main b\""`), when the layout has areas.
    pub fn template_areas(&self) -> Option<String> {
        self.areas.as_ref().map(|rows| {
            rows.iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(" ")
        })
    }
}

fn fr_list(weights: &[f64]) -> String {
    weights
        .iter()
        .map(|w| format!("{}fr", num_js(*w)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A weight as the template string writes it (`1`, `2.5`).
fn num_js(n: f64) -> String {
    crate::util::js_number(n)
}

/// Grid styles for a layout: the track sizes win when their length matches the layout's
/// (a stale override from another shape is ignored).
pub fn grid_styles(def: &LayoutDefinition, track_sizes: Option<&TrackSizes>) -> GridStyles {
    let cols = match track_sizes.and_then(|t| t.cols.as_ref()) {
        Some(c) if c.len() == def.cols.len() => c.clone(),
        _ => def.cols.clone(),
    };
    let rows = match track_sizes.and_then(|t| t.rows.as_ref()) {
        Some(r) if r.len() == def.rows.len() => r.clone(),
        _ => def.rows.clone(),
    };
    let per_cell = def
        .cells
        .iter()
        .map(|c| (c.id.clone(), c.area.clone()))
        .collect();
    GridStyles {
        cols,
        rows,
        areas: def.areas.clone(),
        per_cell,
    }
}

/// The active cell after a layout change: keep it when its slot survives, else fall back
/// to the first slot (the reducer behind `setLayout`; pure for tests).
pub fn active_after_layout(current: Option<&str>, cell_ids: &[String]) -> Option<String> {
    if let Some(c) = current
        && cell_ids.iter().any(|id| id == c)
    {
        return Some(c.to_string());
    }
    cell_ids.first().cloned()
}

/// The identity order after a layout change: shrinking the grid must never pool the
/// ACTIVE chart, so an active identity that would fall past the new slot count moves into
/// the LAST surviving slot; every other identity keeps its relative order.
pub fn order_after_layout(order: &[String], slots: usize, active: Option<&str>) -> Vec<String> {
    let mut next = order.to_vec();
    let Some(active) = active else {
        return next;
    };
    if slots == 0 {
        return next;
    }
    let Some(idx) = next.iter().position(|id| id == active) else {
        return next;
    };
    if idx < slots {
        return next;
    }
    let moved = next.remove(idx);
    next.insert(slots - 1, moved);
    next
}

#[cfg(test)]
mod tests;
