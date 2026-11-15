//! The seam where the interactive user-drawings controller plugs into the shell.
//!
//! In the original, `UserDrawingController` (the toolbar, hit-testing, handles,
//! the placement ghost, the settings popup) is created by the renderer and driven
//! from its frame: the shell asks it for the interleave slices of user drawings
//! that sit inside a pane's series stack, repaints its own layer after the chrome,
//! and dismisses its popup on a resize. The controller is DOM-heavy and ports
//! later; the shell only needs the calls below, so it holds a
//! [`UserDrawingsHook`]. The same object (or a sibling) also serves the core's
//! `DrawingsRendererPort`; hand both to
//! [`NativeRenderer::set_user_drawings`](super::NativeRenderer::set_user_drawings).
//!
//! The hook is called while the shell's state is borrowed (inside a frame), with
//! the scene and coordinate system as a [`FrameView`]. It must paint from the view
//! and must not call back into the renderer synchronously.
//!
//! What the controller reads from the shell instead (the original's constructor
//! callbacks) is exposed on [`NativeRenderer`](super::NativeRenderer):
//! `with_drawing_projector`, `series_look`, `chart_bar_ms`, `timezone`,
//! `price_decimals`, `series_boundaries`, `price_z`, `snap_to_candle`,
//! `snap_mode` / `set_snap_mode`, `set_toolbar_gutter` and `invalidate_data`
//! (`requestScaleUpdate` / `requestDataPaint`).

use std::collections::HashMap;
use std::rc::Rc;

use crate::drawings::{DrawingSeriesGateway, Projector};
use crate::i18n::Translator;
use crate::model::Ohlcv;
use crate::ports::chart_renderer::{LayoutMode, VelaTheme};
use crate::render::canvas::Canvas2d;
use crate::render::core::scene_graph::DrawingSlice;
use crate::render::core::{CoordinateSystem, SceneGraph};
use crate::render::drawings::projector::{PaneView, create_projector};

use super::SeriesLook;

/// What a frame hands the controller: the scene and coordinate system, the live plot
/// theme and what the original's constructor callbacks (`dpr`, `seriesLook`, the
/// canvas) provided.
pub struct FrameView<'a> {
    pub scene: &'a SceneGraph,
    pub coords: &'a CoordinateSystem,
    pub theme: &'a VelaTheme,
    /// The 2D context of the drawings canvas (`None` before mount).
    pub drawings: Option<Rc<dyn Canvas2d>>,
    /// The drawings canvas's backing size in device px (what `canvas.width` / `height`
    /// hold in the original).
    pub backing: (f64, f64),
    /// The look the price series actually paints with (the magnifier's inset mirrors it).
    pub series_look: SeriesLook,
    /// The chart's translator (`None` reads English).
    pub i18n: Option<Rc<dyn Translator>>,
}

impl FrameView<'_> {
    /// Run `f` with the data-to-pixel projector user drawings resolve their
    /// anchors through (the original's `drawingProjector()`).
    pub fn with_projector<R>(
        &self,
        gateway: Option<&dyn DrawingSeriesGateway>,
        f: impl FnOnce(&dyn Projector) -> R,
    ) -> R {
        with_projector(self.scene, self.coords, gateway, f)
    }
}

/// Build the projector over `coords` and the scene's panes and bars.
pub fn with_projector<R>(
    scene: &SceneGraph,
    coords: &CoordinateSystem,
    gateway: Option<&dyn DrawingSeriesGateway>,
    f: impl FnOnce(&dyn Projector) -> R,
) -> R {
    let pane_of = |id: &str| {
        scene.panes.get(id).map(|p| PaneView {
            scale: p.scale,
            bounds: p.bounds,
            collapsed: p.collapsed,
        })
    };
    let pane_id_at_y = |y: f64| pane_node_at_y(scene, y).map(|p| p.id.clone());
    let bars_in_range = |from: f64, to: f64| bars_in_time_range(&scene.bars, from, to);
    match gateway {
        Some(g) => {
            let series = |tf: &str, from: f64, to: f64| g.series_in_range(tf, from, to);
            let projector = create_projector(
                coords,
                &pane_of,
                &pane_id_at_y,
                Some(&bars_in_range),
                Some(&series),
            );
            f(&projector)
        }
        None => {
            let projector =
                create_projector(coords, &pane_of, &pane_id_at_y, Some(&bars_in_range), None);
            f(&projector)
        }
    }
}

/// The pane node whose vertical bounds contain `y` (panes do not overlap).
pub(crate) fn pane_node_at_y(
    scene: &SceneGraph,
    y: f64,
) -> Option<&crate::render::core::scene_graph::PaneNode> {
    scene
        .panes
        .values()
        .find(|p| y >= p.bounds.top && y <= p.bounds.top + p.bounds.height)
}

/// OHLC bars whose open-time falls within `[from, to]` (inclusive): the data a
/// regression (or other statistical) drawing fits against.
pub(crate) fn bars_in_time_range(bars: &[Ohlcv], from: f64, to: f64) -> Vec<Ohlcv> {
    let lo = crate::indicators::classics::math::js_min(from, to);
    let hi = crate::indicators::classics::math::js_max(from, to);
    let mut out = Vec::new();
    for b in bars {
        let t = b.time as f64;
        if t < lo {
            continue;
        }
        if t > hi {
            break;
        }
        out.push(*b);
    }
    out
}

/// The interactive user-drawings controller, as the shell drives it.
pub trait UserDrawingsHook {
    /// Prepaint the user drawings whose z sits inside each pane's series stack
    /// (`prepareSlices`): a pane id to its slices.
    fn prepare_slices(
        &self,
        pane_ids: &[String],
        view: &FrameView,
    ) -> HashMap<String, Vec<DrawingSlice>>;
    /// Repaint the drawings layer (above the chrome, below the crosshair).
    fn render(&self, view: &FrameView);
    /// The plot was resized: dismiss the settings popup.
    fn on_resize(&self) {}
    /// Drop the hover highlight (the `drawingsInteractive` feature went off).
    fn clear_hover(&self) {}
    /// A click on the empty plot ends a (multi-)selection.
    fn deselect(&self) {}
    /// The core's series gateway, for data-driven drawings (the projector's
    /// `series_in_range`).
    fn series_gateway(&self) -> Option<Rc<dyn DrawingSeriesGateway>> {
        None
    }
    /// The chrome theme changed (toolbar and popups re-theme).
    fn set_theme(&self, _chrome_theme: &VelaTheme) {}
    fn set_layout_mode(&self, _mode: LayoutMode) {}
    /// The `historyChords` feature: whether the layer consumes Ctrl+Z/Y.
    fn set_history_chords(&self, _on: bool) {}
    /// The chart's language changed.
    fn relocalize(&self) {}
    fn destroy(&self) {}
}
