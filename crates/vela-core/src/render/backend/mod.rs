//! The seam between the renderer and a drawing backend, and the backend-independent
//! data preparation: candle level of detail, close-only style geometry, color parsing,
//! the layer registry and the volume-profile binning; plus the Canvas 2D backend
//! ([`canvas2d_backend`]).
//!
//! The WebGL2 geometry backend ([`webgl2_backend`], with [`gl::batch`]) paints through the
//! [`Gl`](crate::render::gl::Gl) trait. `supportsWebGL2` of the original's `capabilities` (a
//! probe that creates a canvas) is the host's: it answers by handing out a GL canvas or not
//! (see `NativeHost::create_gl_canvas`).

pub mod candle_lod;
pub mod canvas2d_backend;
pub mod capabilities;
pub mod close_styles;
pub mod gl;
pub mod layers;
pub mod render_backend;
pub mod vpvr_profile;
pub mod webgl2_backend;
