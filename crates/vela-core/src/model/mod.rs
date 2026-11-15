//! The neutral model: the renderer-independent description of what a chart
//! shows. Every layer above (engines, renderers, hosts) speaks these types.
//!
//! Not yet ported from the original `model/`: `strategy` and `volume-layers`.

pub mod drawings;
pub mod identity;
pub mod indicator;
pub mod inputs;
pub mod ohlcv;
pub mod patch;
pub mod scene;
pub mod series;
pub mod strategy;
pub mod trades;
pub mod volume_layers;

pub use drawings::*;
pub use identity::stable_series_id;
pub use indicator::*;
pub use inputs::*;
pub use ohlcv::{Millis, Ohlcv};
pub use patch::*;
pub use scene::*;
pub use series::*;
pub use trades::*;
