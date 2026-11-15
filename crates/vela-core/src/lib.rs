//! Renderer-neutral core of the Vela charting library, ported from the
//! TypeScript original (<https://github.com/LuxAlgo/Vela>, Apache-2.0).
//!
//! Nothing here touches a DOM or a canvas: the crate is the data model, the
//! classic indicator catalog and the user drawing tools. The one read of the
//! wall clock is the creation time of a drawing built without one.

pub mod chart_types;
pub mod controls;
pub mod data;
pub mod data_unavailable;
pub mod drawings;
pub mod engine;
pub mod events;
pub mod format;
pub mod i18n;
pub mod indicators;
pub mod intl;
pub mod locales;
pub mod marks;
pub mod model;
pub mod options;
pub mod palette;
pub mod plugin;
pub mod ports;
pub mod price_styles;
pub mod render;
pub mod renderer_defaults;
pub mod runtime;
pub mod script_run;
pub mod state;
pub mod theme;
pub mod timezones;
pub mod ui;
pub mod util;
pub mod vela;
pub mod visible_range;
pub mod widget;
pub mod workspace;
