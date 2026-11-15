//! Renderer pieces shared by the native renderer's layers: drawing geometry, text
//! wrapping, the table overlay's plan, trade markers and the marks display state.
//!
//! Left for the browser crate or later stages:
//! - `theme-tokens`: themes a DOM element with the UI kit's tokens (`applyChromeTokens`);
//!   it is DOM work only.
//! - `InputsUI`, `IndicatorInputsDialog`, `TableOverlay` (the DOM overlay itself; its
//!   planning half is [`table_overlay`]), `dom-raster` and `chrome-tooltip`: DOM.
//! - `DrawingSceneRenderer` and `TableCanvasRenderer` are [`drawing_scene_renderer`] and
//!   [`table_canvas_renderer`].

pub mod drawing_geometry;
pub mod drawing_scene_renderer;
pub mod marks_state;
pub mod table_canvas_renderer;
pub mod table_overlay;
pub mod text_wrap;
pub mod trade_markers;

pub use drawing_scene_renderer::{
    BarHighLow, DrawingPriceRange, DrawingSceneRenderer, DrawingSet, DrawingSetSource,
    LabelTipRegion, LayerDeps, drawing_set_empty, model_drawing_set,
};
pub use table_canvas_renderer::{TablePaintArgs, paint_table};
