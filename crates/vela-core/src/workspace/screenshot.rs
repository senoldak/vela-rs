//! Layout screenshot compositing: PURE geometry plus a thin canvas blit
//! (`screenshot.ts`).
//!
//! Each cell already knows how to rasterize itself
//! ([`NativeRenderer::screenshot_pile`](crate::render::native::NativeRenderer::screenshot_pile)
//! returns the canvases in compositing order). The workspace chrome's job is to place
//! those rasters onto one image in the same positions the grid shows, with the seam color
//! in the gaps. Hidden cells (maximized siblings) are dropped before they get here.
//!
//! # Left for the browser crate
//!
//! `compositeLayoutScreenshot` creates a `<canvas>` and encodes `toDataURL('image/png')`;
//! `triggerPngDownload` clicks an `<a download>`. Here [`composite_layout_screenshot`]
//! paints on a [`Canvas2d`] the host created at [`LayoutShotFrame::backing_size`], and the
//! host encodes it and downloads it (see
//! [`WorkspaceHost::screenshot_surface`](super::WorkspaceHost::screenshot_surface)).

use crate::indicators::classics::math::{js_max, js_round};
use crate::render::canvas::{Canvas2d, CanvasImage, Paint};

/// A cell's box in CSS pixels, origin at the grid's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShotRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A cell's raster and where it goes. The original hands over one composited canvas per
/// cell (`screenshotCanvas()`); the native renderer's pile is several, painted back to
/// front over the plot background (see `NativeRenderer::screenshot_pile`).
#[derive(Clone)]
pub struct LayoutShotTile {
    /// The plot background painted first, when the pile has one.
    pub background: Option<String>,
    /// The canvases to blit, back to front, each stretched over `rect`.
    pub sources: Vec<CanvasImage>,
    pub rect: ShotRect,
}

/// The grid's CSS box and the device-pixel scale of the export.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutShotFrame {
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
    /// Computed background of the grid: paints the visible seams.
    pub gap_color: String,
}

impl LayoutShotFrame {
    /// The export's backing size in device pixels (at least 1x1).
    pub fn backing_size(&self) -> (f64, f64) {
        let dpr = self.effective_dpr();
        (
            js_max(1.0, js_round(self.width * dpr)),
            js_max(1.0, js_round(self.height * dpr)),
        )
    }

    fn effective_dpr(&self) -> f64 {
        if self.dpr > 0.0 { self.dpr } else { 1.0 }
    }
}

/// Pixel destination of a CSS tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DestRect {
    pub dx: f64,
    pub dy: f64,
    pub dw: f64,
    pub dh: f64,
}

/// Pixel dest of a CSS tile, rounded so adjacent cells share an edge (no 1px gap).
pub fn dest_rect(tile: &ShotRect, dpr: f64) -> DestRect {
    let dx = js_round(tile.x * dpr);
    let dy = js_round(tile.y * dpr);
    DestRect {
        dx,
        dy,
        dw: js_round((tile.x + tile.width) * dpr) - dx,
        dh: js_round((tile.y + tile.height) * dpr) - dy,
    }
}

/// A cell's viewport box, for [`tiles_from_cell_rects`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellBox {
    pub hidden: bool,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// The grid's viewport box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridBox {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// CSS tiles for every cell that is on screen (not hidden, non-zero box).
pub fn tiles_from_cell_rects(grid: &GridBox, cells: &[CellBox]) -> Vec<ShotRect> {
    if grid.width <= 0.0 || grid.height <= 0.0 {
        return Vec::new();
    }
    cells
        .iter()
        .filter(|c| !c.hidden && c.width > 0.0 && c.height > 0.0)
        .map(|c| ShotRect {
            x: c.left - grid.left,
            y: c.top - grid.top,
            width: c.width,
            height: c.height,
        })
        .collect()
}

/// Composite `tiles` onto `canvas` (created at [`LayoutShotFrame::backing_size`]). Returns
/// whether anything was painted: an empty or zero-size frame, or a tile list that painted
/// nothing, returns `false` (the original returns a null data URL), and the host then has
/// nothing to encode.
pub fn composite_layout_screenshot(
    canvas: &dyn Canvas2d,
    frame: &LayoutShotFrame,
    tiles: &[LayoutShotTile],
) -> bool {
    composite_layout_screenshot_with(canvas, frame, tiles, &|_, _, _| {})
}

/// Which DOM overlays a tile's hook paints: those that sit beneath the canvases (the symbol
/// watermark) right after the tile's background, or those above them (legends, the status
/// line, the attribution mark) after its last canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayPhase {
    Under,
    Over,
}

/// [`composite_layout_screenshot`] with a hook the host uses to rasterize DOM chrome into
/// each tile: `overlay(tile_index, phase, dest)` runs once per painted tile and phase, in
/// the order the original draws them (under, canvases, over).
pub fn composite_layout_screenshot_with(
    canvas: &dyn Canvas2d,
    frame: &LayoutShotFrame,
    tiles: &[LayoutShotTile],
    overlay: &dyn Fn(usize, OverlayPhase, &DestRect),
) -> bool {
    if frame.width <= 0.0 || frame.height <= 0.0 || tiles.is_empty() {
        return false;
    }
    let dpr = frame.effective_dpr();
    let (w, h) = frame.backing_size();
    canvas.set_fill_style(&Paint::Color(frame.gap_color.clone()));
    canvas.fill_rect(0.0, 0.0, w, h);
    let mut painted = 0;
    for (index, tile) in tiles.iter().enumerate() {
        if tile.rect.width <= 0.0 || tile.rect.height <= 0.0 {
            continue;
        }
        let d = dest_rect(&tile.rect, dpr);
        if d.dw <= 0.0 || d.dh <= 0.0 {
            continue;
        }
        if let Some(background) = &tile.background {
            canvas.set_fill_style(&Paint::Color(background.clone()));
            canvas.fill_rect(d.dx, d.dy, d.dw, d.dh);
        }
        overlay(index, OverlayPhase::Under, &d);
        for source in &tile.sources {
            canvas.draw_image(source, d.dx, d.dy, d.dw, d.dh);
        }
        overlay(index, OverlayPhase::Over, &d);
        painted += 1;
    }
    painted > 0
}

#[cfg(test)]
mod tests;
