//! The native renderer, ported from the original `src/renderers`.
//!
//! What is here is platform-neutral: the scene math, and painters that draw
//! through the [`canvas::Canvas2d`] abstraction. The DOM-bound parts of the
//! original (dialogs, toolbars, pointer and keyboard wiring, canvas
//! management) belong to the browser crate.

pub mod backdrop;
pub mod backend;
pub mod canvas;
pub mod chrome;
pub mod core;
pub mod drawings;
pub mod gl;
pub mod native;
pub mod shared;
pub mod volume;
pub mod vpvr;
