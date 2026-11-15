//! The native renderer's chrome: axis tick generation and labels, the right-gutter
//! layout, countdown text, time zones, the timeline-mark lane's layout, the settings
//! dialog's visibility policy, and the painters of the axes, the crosshair and the
//! mark lane ([`chrome_renderer`], [`crosshair_renderer`], [`marks_paint`]).
//!
//! Left for the browser crate: the DOM pieces of the original `native/chrome` folder
//! (`marks/MarkPopover`, `marks/MarkIconRaster`, `AttributionMark`, `AxisScaleButtons`,
//! `PaneControls`, `LabelTooltip`, `SettingsDialog`).

pub mod axis_layout;
pub mod chrome_renderer;
pub mod contrast;
pub mod countdown;
pub mod crosshair_renderer;
pub mod marks_layout;
pub mod marks_paint;
pub mod price_axis_ticks;
pub mod settings_visibility;
pub mod ticks;
pub mod tz;
pub mod vela_logos;
