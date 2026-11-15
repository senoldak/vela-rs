//! Painting and hit-testing of drawings: the user's drawing tools and the
//! drawing objects indicators emit.

pub mod hit_tester;
pub mod indicator_slices;
pub mod interaction;
pub mod interaction_logic;
pub mod keys;
pub mod measure_overlay;
pub mod painter;
pub mod projector;
pub mod user_controller;

pub use hit_tester::{HIT_TOLERANCE, deletable_selection, delete_targets, top_drawing_at};
pub use indicator_slices::{
    IndicatorDrawingSlices, SliceSurface, SliceSurfaceFactory, indicator_slice_key, merge_slices,
};
pub use interaction::{DrawingInteraction, InteractionDeps, SnapMarker};
pub use keys::{
    DrawingKeyAction, DrawingKeyContext, DrawingKeyEvent, is_editing_text, key_to_drawing_action,
};
pub use measure_overlay::{MeasureOverlay, MeasurePoint, MeasurePoints};
pub use projector::{NativeProjector, PaneView, create_projector};
pub use user_controller::{
    DrawingsUiEvent, MagnifierMenuRequest, PopupEvent, PopupRequest, TextEditorEvent,
    TextEditorKind, TextEditorSpec, ToolbarUiState, UserDrawingController,
};
