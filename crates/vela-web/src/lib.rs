//! Browser (WebAssembly) layer of the Vela port.
//!
//! `vela-core` is platform-neutral: wherever the original TypeScript reaches
//! for a browser API, core asks a trait instead. This crate implements those
//! traits on top of the browser through `wasm-bindgen`.

pub mod app;
mod binding;
pub mod canvas;
pub mod chart;
pub mod chrome_ui;
pub mod dialogs;
pub mod dom;
mod drawings_api;
pub mod drawings_ui;
pub mod gl;
pub mod host;
mod icons;
pub mod input;
pub mod intl;
pub mod locales;
mod logos;
#[cfg(feature = "parity")]
pub mod parity;
pub mod platform;
pub mod raster;
pub mod shell_side;
pub mod shell_top;
pub mod themes;
pub mod vela_api;
pub mod workspace_ui;

pub use app::VelaApp;
pub use canvas::BrowserCanvas;
pub use chart::VelaChart;
pub use host::BrowserHost;
pub use intl::BrowserIntl;
pub use platform::BrowserPlatform;
pub use workspace_ui::{VelaWidget, VelaWorkspace, VelaWorkspaceCell};

fn date_now() -> f64 {
    js_sys::Date::now()
}

/// Wire core to the browser: installs the wall clock core reads for a new
/// drawing's creation time. Call once at startup; later calls do nothing.
pub fn install() {
    vela_core::drawings::set_clock(date_now);
}
