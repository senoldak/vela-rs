//! Workspace SPLITTERS: draggable dividers between grid tracks (`splitters.ts`). The math
//! is pure; the strips themselves are the browser's.
//!
//! # Left for the browser crate
//!
//! The original's `SplitterLayer` owns DOM strips and pointer listeners. Here the same
//! decisions are data: [`splitter_strips`] is what `SplitterLayer.layout()` computes (one
//! placement per seam segment of each internal boundary, in CSS px relative to the grid
//! container; the browser creates, positions and cursors the divs and removes the
//! surplus) and [`SplitterDrag`] is the drag state `onDown`/`move` keep (the browser
//! feeds it the pointer's position along the axis and commits the weights it returns
//! through [`Workspace::apply_tracks`](super::Workspace::apply_tracks)). A double-click
//! calls [`Workspace::reset_tracks`](super::Workspace::reset_tracks).

use crate::indicators::classics::math::{js_max, js_min};

/// Smallest share of the total weight one track may shrink to (10%).
const MIN_TRACK_FRAC: f64 = 0.1;

/// Strip thickness: the visible seam is the grid gap; this is the grab target.
pub const HIT_PX: f64 = 8.0;

/// A grid axis: `Cols` boundaries are vertical seams, `Rows` boundaries horizontal ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    Cols,
    Rows,
}

impl Axis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cols => "cols",
            Self::Rows => "rows",
        }
    }
}

/// Even weights for `n` tracks (the double-click reset).
pub fn even_tracks(n: usize) -> Vec<f64> {
    vec![1.0; n]
}

/// Resize the boundary between tracks `index` and `index + 1` by a pixel delta. The two
/// neighbors trade weight (their sum, and every other track, is preserved); both are
/// clamped so neither drops under 10% of the total. `size_px` is the container's content
/// size along the axis (gaps excluded).
pub fn resize_tracks(weights: &[f64], index: usize, delta_px: f64, size_px: f64) -> Vec<f64> {
    let mut out = weights.to_vec();
    let (Some(&a), Some(&b)) = (out.get(index), out.get(index + 1)) else {
        return out;
    };
    // `sizePx <= 0` is false for NaN, so a NaN size goes on to produce NaN weights, as it does there.
    if size_px <= 0.0 {
        return out;
    }
    let total: f64 = out.iter().sum();
    let min = total * MIN_TRACK_FRAC;
    let mut delta_fr = (delta_px / size_px) * total;
    delta_fr = js_max(delta_fr, min - a); // a never below min
    delta_fr = js_min(delta_fr, b - min); // b never below min
    out[index] = a + delta_fr;
    out[index + 1] = b - delta_fr;
    out
}

/// Pixel centers of the INTERNAL track boundaries (`n - 1` entries): where the divider
/// strips sit. Cumulative weights over the gap-corrected content size.
pub fn track_offsets(weights: &[f64], size_px: f64, gap_px: f64) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || weights.is_empty() {
        return Vec::new();
    }
    let content = size_px - gap_px * (weights.len() - 1) as f64;
    let mut out = Vec::with_capacity(weights.len() - 1);
    let mut acc = 0.0;
    for (i, w) in weights.iter().take(weights.len() - 1).enumerate() {
        acc += (w / total) * content;
        out.push(acc + gap_px * i as f64 + gap_px / 2.0); // center of the gap after track i
    }
    out
}

/// The stretches of one internal boundary that are REAL seams. `grid[r][c]` is the slot
/// occupying that track (see `occupancy_grid`); a spanning cell makes the two neighboring
/// tracks equal, and no strip may cover that stretch (it would sit on top of a chart, not
/// between two). Returns inclusive track ranges along the boundary's own direction: rows
/// for a `Cols` boundary, columns for a `Rows` one.
pub fn seam_segments(grid: &[Vec<String>], axis: Axis, index: usize) -> Vec<(usize, usize)> {
    let count = match axis {
        Axis::Cols => grid.len(),
        Axis::Rows => grid.first().map_or(0, Vec::len),
    };
    let cell = |r: usize, c: usize| grid.get(r).and_then(|row| row.get(c));
    let is_seam = |k: usize| match axis {
        Axis::Cols => cell(k, index) != cell(k, index + 1),
        Axis::Rows => cell(index, k) != cell(index + 1, k),
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    for k in 0..count {
        if !is_seam(k) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 + 1 == k => last.1 = k,
            _ => out.push((k, k)),
        }
    }
    out
}

/// The pixel extent of tracks `from`..=`to` along one axis. Interior ends extend halfway
/// into the adjacent gap so strips meet cleanly at crossings; container ends run flush to
/// the edge.
pub fn segment_span_px(
    weights: &[f64],
    size_px: f64,
    gap_px: f64,
    from: usize,
    to: usize,
) -> (f64, f64) {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || weights.is_empty() {
        return (0.0, 0.0);
    }
    let content = size_px - gap_px * (weights.len() - 1) as f64;
    let start_of = |k: usize| -> f64 {
        weights
            .iter()
            .take(k)
            .map(|w| (w / total) * content)
            .sum::<f64>()
            + gap_px * k as f64
    };
    let start = if from == 0 {
        0.0
    } else {
        start_of(from) - gap_px / 2.0
    };
    let end = if to == weights.len() - 1 {
        size_px
    } else {
        start_of(to + 1) - gap_px / 2.0
    };
    (start, end)
}

/// One divider strip: where it sits, in CSS px relative to the grid container, and what a
/// drag on it resizes. The cursor is `col-resize` for `Axis::Cols` and `row-resize` for
/// `Axis::Rows`.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitterStrip {
    pub axis: Axis,
    /// The boundary between tracks `index` and `index + 1`.
    pub index: usize,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

fn round(n: f64) -> f64 {
    crate::indicators::classics::math::js_round(n)
}

/// The strips for the current tracks, container size and slot occupancy: one per seam
/// SEGMENT of each internal column/row boundary (never the boundary's full length: a cell
/// spanning the boundary must keep receiving the pointer there). Recompute whenever the
/// layout, the track weights or the container size change.
pub fn splitter_strips(
    cols: &[f64],
    rows: &[f64],
    grid: &[Vec<String>],
    width: f64,
    height: f64,
    gap_px: f64,
) -> Vec<SplitterStrip> {
    let mut out = Vec::new();
    for (i, x) in track_offsets(cols, width, gap_px).into_iter().enumerate() {
        for (from, to) in seam_segments(grid, Axis::Cols, i) {
            let (start, end) = segment_span_px(rows, height, gap_px, from, to);
            out.push(SplitterStrip {
                axis: Axis::Cols,
                index: i,
                left: round(x - HIT_PX / 2.0),
                top: round(start),
                width: HIT_PX,
                height: round(end - start),
            });
        }
    }
    for (i, y) in track_offsets(rows, height, gap_px).into_iter().enumerate() {
        for (from, to) in seam_segments(grid, Axis::Rows, i) {
            let (start, end) = segment_span_px(cols, width, gap_px, from, to);
            out.push(SplitterStrip {
                axis: Axis::Rows,
                index: i,
                left: round(start),
                top: round(y - HIT_PX / 2.0),
                width: round(end - start),
                height: HIT_PX,
            });
        }
    }
    out
}

/// A drag in progress on one strip (what `SplitterLayer.onDown` captures).
#[derive(Clone, Debug, PartialEq)]
pub struct SplitterDrag {
    axis: Axis,
    index: usize,
    start_px: f64,
    start_weights: Vec<f64>,
    size_px: f64,
}

impl SplitterDrag {
    /// Begin a drag. `start_px` is the pointer's position along the axis (clientX for
    /// `Cols`, clientY for `Rows`); `container_px` the container's size along it; `weights`
    /// the axis's current weights. The content size excludes the gaps.
    pub fn begin(
        axis: Axis,
        index: usize,
        start_px: f64,
        weights: &[f64],
        container_px: f64,
        gap_px: f64,
    ) -> Self {
        Self {
            axis,
            index,
            start_px,
            start_weights: weights.to_vec(),
            size_px: container_px - gap_px * weights.len().saturating_sub(1) as f64,
        }
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// The weights after the pointer moved to `pointer_px`.
    pub fn update(&self, pointer_px: f64) -> Vec<f64> {
        resize_tracks(
            &self.start_weights,
            self.index,
            pointer_px - self.start_px,
            self.size_px,
        )
    }
}

#[cfg(test)]
mod tests;
