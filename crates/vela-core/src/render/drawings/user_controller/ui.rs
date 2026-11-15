//! The seam between the user-drawings controller and the DOM widgets it drives in the
//! original: the docked toolbar, the settings popup, the inline text editor and the
//! magnifier's timeframe menu.
//!
//! The controller owns every decision (what is selected, which popup to show for which
//! drawings, where the inline editor sits and how it is styled); the browser crate owns the
//! elements. The controller reports through ONE host method,
//! [`NativeHost::drawings_ui`](crate::render::native::NativeHost::drawings_ui), with a
//! [`DrawingsUiEvent`]; the DOM answers by calling the public methods of
//! [`UserDrawingController`](super::UserDrawingController) (`toolbar_*`, `popup_*`,
//! `text_editor_*`, `magnifier_timeframe_picked`). Each event documents what the browser
//! has to do. The host is called with no controller state borrowed, but possibly with the
//! renderer's state borrowed (from inside a frame): a handler must not call back into the
//! renderer or the controller synchronously (queue it onto a microtask or the next event).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::drawings::{Bounds, SnapMode};

/// Everything one event can tell the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawingsUiEvent {
    /// The docked toolbar changed (any field): re-render it from this snapshot.
    Toolbar(ToolbarUiState),
    /// The inline text editor opened, moved or closed.
    TextEditor(TextEditorEvent),
    /// The settings popup (the floating bar and its full panel) opened or closed.
    Popup(PopupEvent),
    /// Open the on-chart magnifier chip's timeframe menu.
    MagnifierMenu(MagnifierMenuRequest),
}

/// A full snapshot of the docked drawing toolbar (the original `DrawingToolbar`'s state).
///
/// The browser renders the bar from `definition` (the core's toolbar definition: groups,
/// sections, tools with SVG icons) and reflects the other fields on its buttons. User actions
/// go back through `toolbar_arm`, `toolbar_set_snap_mode`, `toolbar_toggle_measure`,
/// `toolbar_toggle_eraser`, `toolbar_favorite`, `toolbar_stay_mode` and
/// `toolbar_collapsed`. The left gutter the bar needs is reserved by the controller itself
/// through the renderer (`gutter`, 44 px, or 16 px while collapsed, 0 while hidden); the
/// browser only has to size the bar to it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolbarUiState {
    /// The core's `ToolbarDefinition` (opaque JSON, `Null` until the core pushes one).
    pub definition: Value,
    /// Whether the bar shows: the host's intent AND not the mobile layout (mobile
    /// suppresses the docked bar; the shell's own tool drawer takes over).
    pub visible: bool,
    /// The armed tool (highlights its button), `None` for the pointer.
    pub active_tool: Option<String>,
    /// The favorite tools (flyout stars).
    pub favorites: Vec<String>,
    /// Pre-formatted shortcut hints per tool, shown in the flyouts.
    pub shortcuts: BTreeMap<String, String>,
    /// The sticky magnet mode (the 3-state button).
    pub magnet: SnapMode,
    /// Stay-in-drawing-mode.
    pub stay_mode: bool,
    /// The measure ruler is armed.
    pub measure_active: bool,
    /// The eraser is armed.
    pub eraser_active: bool,
    /// The left gutter the plot reserves for the bar, px.
    pub gutter: f64,
}

/// What a text drawing's inline editor is: a callout fills its bubble, a text label sits
/// exactly where the glyphs paint inside a thin frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEditorKind {
    Callout,
    Label,
}

/// The geometry and look of the inline editor: the original's `cssText`, as data.
///
/// The browser creates one absolutely positioned `<textarea>` over the plot with
/// `position:absolute; z-index:24; box-sizing:border-box; resize:none; overflow:hidden;
/// margin:0; outline:none; pointer-events:auto; text-align:left`, the fields below as its
/// style, `spellcheck=false` and `placeholder`, `value = text`, the caret at the END with
/// nothing selected, and focuses it. It also: stops `pointerdown` and `keydown`
/// propagation; reports every `input` as `text_editor_input(value)`; inserts a line break on
/// a plain Enter itself (`setRangeText('\n', ..)`, then `text_editor_input`) and calls
/// `text_editor_commit()` on Ctrl/Cmd+Enter and `text_editor_cancel()` on Escape; on `blur`
/// calls `text_editor_blur(into_popup)` where `into_popup` says whether focus moved into
/// the settings popup.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEditorSpec {
    /// The edited drawing.
    pub id: String,
    pub kind: TextEditorKind,
    /// Plot-relative px, already rounded.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The text to show (the drawing's own text when the editor opens; the typed text on a
    /// re-layout, which the browser may ignore: it owns the live value).
    pub text: String,
    /// Shown while the text is empty ("Enter Text", in the chart's language).
    pub placeholder: String,
    /// CSS `font` shorthand.
    pub font: String,
    pub color: String,
    /// CSS background (`transparent` for a label; an opaque blend for a callout).
    pub background: String,
    pub border_color: String,
    pub border_width: f64,
    pub border_radius: f64,
    /// CSS padding, px: vertical, horizontal.
    pub padding: (f64, f64),
    /// CSS `line-height` in px (a label only).
    pub line_height: Option<f64>,
    /// CSS `white-space: pre` (a label only).
    pub preserve_whitespace: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextEditorEvent {
    /// Create (replacing any open one) the editor described by the spec and focus it.
    Open(TextEditorSpec),
    /// Re-apply the geometry and look (typing, panning and zooming move the anchor).
    Layout(TextEditorSpec),
    /// Remove the editor element.
    Close,
}

/// What the settings popup is for.
#[derive(Clone, Debug, PartialEq)]
pub struct PopupRequest {
    /// The drawings it edits: one id, or the whole multi-selection.
    pub ids: Vec<String>,
    /// The multi-selection bar (the controls all of `ids` share) rather than one drawing's
    /// own.
    pub multi: bool,
    /// The pixel box to float the bar clear of (the drawing's, or the union of the
    /// selection's), `None` when none is resolvable.
    pub anchor: Option<Bounds>,
}

/// The settings popup.
///
/// On `Open` the browser shows the floating bar for `ids`, reading each drawing's live state
/// through `UserDrawingController::drawing_doc(id)` (the schema and values come from the
/// model's `Drawing::schema()` / `serialize()`, rebuilt by `deserialize`), and routes the
/// controls to `popup_patch`, `popup_set_locked`, `popup_reorder`, `popup_duplicate`,
/// `popup_reset_settings`, `popup_remove`, `popup_restore`, `popup_tool_templates`,
/// `popup_save_template`, `popup_remove_template`, `popup_apply_tool_settings` and
/// `popup_preview`. An `Open` for the same single drawing while its full panel is open keeps
/// the panel. When the popup closes ITSELF (an outside press, or Escape in a text field of
/// its own) the browser calls `popup_dismissed(modifier)` once, with `modifier` true when
/// the press carried Shift, Ctrl or Cmd; a close the controller asked for (`Close`) needs no
/// report.
#[derive(Clone, Debug, PartialEq)]
pub enum PopupEvent {
    Open(PopupRequest),
    Close,
    /// The model was re-synced under the open popup: refresh the values its panel shows.
    SyncPanel,
    /// Open the full settings panel (the double-click path).
    OpenPanel,
}

/// The magnifier's on-chart timeframe chip was pressed.
///
/// The browser opens a menu of the timeframe choices (the model's
/// `magnifier_timeframe_options()`, `current` checked) anchored on `rect` and reports the
/// pick with `magnifier_timeframe_picked(id, value)`.
#[derive(Clone, Debug, PartialEq)]
pub struct MagnifierMenuRequest {
    pub id: String,
    pub rect: Bounds,
    pub current: String,
}
