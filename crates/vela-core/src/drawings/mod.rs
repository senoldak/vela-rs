//! User drawings: the model a chart's drawing tools share.
//!
//! - [`drawing`]: the [`Drawing`] trait, the shared [`DrawingBase`] state and
//!   the serialized record.
//! - [`types`]: every built-in tool.
//! - [`registry`]: the catalog of types and their factories.
//! - [`store`], [`history`], [`document`]: the source of truth, undo/redo and
//!   the persistence envelope.
//! - [`geometry`], [`hittest`]: the data→pixel seam and the pure pixel math.
//!
//! Not ported from the original `core/drawings/`: the interaction controller
//! (`DrawingController`), the renderer port (`port.ts`) and the toolbar
//! definition (`toolbar.ts`). They sit on the renderer and event layers,
//! which this crate does not have yet.

pub mod controller;
pub mod defaults;
pub mod document;
pub mod drawing;
pub mod effective_color;
pub mod fib_presets;
pub mod geometry;
pub mod history;
pub mod hittest;
pub mod level_palette;
pub mod projector;
pub mod registry;
pub mod schema;
pub mod series;
pub mod store;
pub mod style;
pub mod toolbar;
pub mod types;
pub mod visibility;

pub use defaults::*;
pub use document::*;
pub use drawing::*;
pub use geometry::*;
pub use history::*;
pub use hittest::*;
pub use projector::LinearProjector;
pub use registry::*;
pub use schema::*;
pub use series::*;
pub use store::*;
pub use style::*;
pub use visibility::*;
