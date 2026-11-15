//! Renderer-neutral models for script drawing objects (`line.new`, `box.new`, …).
//! Coordinates stay in their script form, tagged by [`DrawingXLoc`]: the
//! renderer converts a bar index or epoch-ms time to a pixel via the time scale.

use super::series::LineStyle;

/// How a drawing's x-coordinates are interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawingXLoc {
    BarIndex,
    BarTime,
}

/// Which side(s) the drawing runs out to the chart edge.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum DrawingExtend {
    #[default]
    None,
    Left,
    Right,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxTextSize {
    Auto,
    Tiny,
    Small,
    Normal,
    Large,
    Huge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxHAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxVAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoxFontFamily {
    Default,
    Monospace,
}

/// A line. `x1/x2` are bar indices or epoch ms (per `xloc`); `y1/y2` are prices.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingLine {
    pub id: String,
    pub pane_id: String,
    pub xloc: DrawingXLoc,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub extend: DrawingExtend,
    /// Stroke color. `None` → the renderer's default foreground color.
    pub color: Option<String>,
    /// The line exists but is not stroked (e.g. a linefill anchor).
    pub invisible: bool,
    /// Line width in px (uncapped, unlike a series line width).
    pub width: f64,
    pub style: LineStyle,
    pub arrow_left: bool,
    pub arrow_right: bool,
    pub overlay: bool,
}

/// A box. `left/right` follow `xloc`; `top/bottom` are prices.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingBox {
    pub id: String,
    pub pane_id: String,
    pub xloc: DrawingXLoc,
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub extend: DrawingExtend,
    /// Fill color (may carry alpha). `None` → no fill.
    pub bg_color: Option<String>,
    /// Border color. `None` → no border.
    pub border_color: Option<String>,
    pub border_width: f64,
    pub border_style: LineStyle,
    pub text: Option<String>,
    /// Text color. `None` → auto-contrast against the fill.
    pub text_color: Option<String>,
    pub text_size: BoxTextSize,
    pub h_align: BoxHAlign,
    pub v_align: BoxVAlign,
    /// Wrap to the box width; otherwise single line per `\n`.
    pub wrap: bool,
    pub font_family: BoxFontFamily,
    pub bold: bool,
    pub italic: bool,
    pub overlay: bool,
}

/// The bubble/pointer variants and the point-marker shapes of a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelStyle {
    LabelUp,
    LabelDown,
    LabelLeft,
    LabelRight,
    LabelCenter,
    LabelLowerLeft,
    LabelLowerRight,
    LabelUpperLeft,
    LabelUpperRight,
    Circle,
    Square,
    Diamond,
    Flag,
    ArrowUp,
    ArrowDown,
    TriangleUp,
    TriangleDown,
    Cross,
    XCross,
    TextOutline,
    None,
}

/// Where a label/marker anchors vertically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelYLoc {
    Price,
    AboveBar,
    BelowBar,
    Top,
    Bottom,
}

/// A label. `x` follows `xloc`; `y` is a price (used when `yloc` is `Price`).
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingLabel {
    pub id: String,
    pub pane_id: String,
    pub xloc: DrawingXLoc,
    pub x: f64,
    pub y: f64,
    pub yloc: LabelYLoc,
    pub text: Option<String>,
    pub style: LabelStyle,
    /// Bubble / marker color. `None` → renderer default.
    pub color: Option<String>,
    pub text_color: Option<String>,
    pub size: BoxTextSize,
    pub text_align: BoxHAlign,
    pub tooltip: Option<String>,
    pub font_family: BoxFontFamily,
    pub bold: bool,
    pub italic: bool,
    /// Render text only (no bubble/shape fill).
    pub no_fill: bool,
    pub overlay: bool,
}

/// One vertex of a polyline. `x` follows `xloc`; `price` is y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolylinePoint {
    pub xloc: DrawingXLoc,
    pub x: f64,
    pub price: f64,
}

/// A multi-point path, optionally curved and/or closed.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingPolyline {
    pub id: String,
    pub pane_id: String,
    pub points: Vec<PolylinePoint>,
    pub curved: bool,
    pub closed: bool,
    /// Stroke color. `None` → no stroke.
    pub line_color: Option<String>,
    /// Fill color (closed paths). `None` → no fill.
    pub fill_color: Option<String>,
    pub line_width: f64,
    pub line_style: LineStyle,
    pub arrow_left: bool,
    pub arrow_right: bool,
    pub overlay: bool,
}

/// The band between two lines.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingLinefill {
    pub id: String,
    pub pane_id: String,
    pub line1: DrawingLine,
    pub line2: DrawingLine,
    /// Fill color. `None` → nothing drawn.
    pub color: Option<String>,
    pub overlay: bool,
}

/// Which chart corner/edge a table anchors to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TablePosition {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// A table cell's text size: a named size, or a raw pixel value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TableTextSize {
    Named(BoxTextSize),
    Px(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub text: Option<String>,
    pub text_color: Option<String>,
    pub bg_color: Option<String>,
    pub h_align: BoxHAlign,
    pub v_align: BoxVAlign,
    pub text_size: TableTextSize,
    pub font_family: BoxFontFamily,
    pub tooltip: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Cell width as a percent of the pane's width (`None`/0 = size to content).
    pub width: Option<f64>,
    /// Cell height as a percent of the pane's height (`None`/0 = size to content).
    pub height: Option<f64>,
    /// A non-origin cell absorbed by a merge region → not rendered. Renderers
    /// must resolve visibility against `DrawingTable::merges`, not this flag alone.
    pub merged: bool,
}

/// A merged-cell region (inclusive, in column/row coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableMerge {
    pub start_col: usize,
    pub start_row: usize,
    pub end_col: usize,
    pub end_row: usize,
}

/// A grid anchored to a chart corner.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingTable {
    pub id: String,
    pub pane_id: String,
    pub position: TablePosition,
    pub columns: usize,
    pub rows: usize,
    pub bg_color: Option<String>,
    pub frame_color: Option<String>,
    pub frame_width: f64,
    pub border_color: Option<String>,
    pub border_width: f64,
    /// Row-major: `cells[row][col]`; entries may be `None` (empty cell).
    pub cells: Vec<Vec<Option<TableCell>>>,
    pub merges: Vec<TableMerge>,
    pub overlay: bool,
}
