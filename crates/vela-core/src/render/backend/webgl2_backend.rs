//! The hand-rolled WebGL2 GEOMETRY backend (L0). Same [`RenderBackend`] seam as the
//! canvas2d backend: it consumes the retained scene and the coordinate system and
//! draws bgcolor, fills, candles, series and hline. Everything else stays on canvas2d
//! layers: axes, drawings and the crosshair on the chrome/cursor layers above, the
//! grid and session highlights on the backdrop canvas below.
//!
//! Design: ONE per-vertex-color triangle pipeline. Every primitive decomposes into
//! colored triangles (rects, quads, line quads, fans) in CSS-pixel space (see
//! [`Batch`]); a vertical gradient is a quad with different top/bottom vertex colors.
//! Geometry is built each frame, culled to the visible bar range (bounded by the
//! visible bars, not the total), and drawn per pane under `gl.scissor`. The canvas2d
//! backend is the permanent fallback.
//!
//! The GL call stream (calls, order, arguments), the shader sources and the float
//! packing are the original's; [`RecordingGl`](crate::render::gl::RecordingGl) records
//! them, and a later check replays the original on a recording JavaScript context.
//!
//! Left for the browser crate: the original's `mount(canvas)` calls
//! `canvas.getContext('webgl2', { alpha: true, antialias: false, depth: false,
//! stencil: false, premultipliedAlpha: true })` and hooks `webglcontextlost` /
//! `webglcontextrestored` on the canvas. Here [`WebGL2Backend::mount`] takes the
//! context as a shared [`Gl`] and the backing-store size as values (kept current with
//! [`set_backing_size`](WebGL2Backend::set_backing_size)). The context attributes are
//! the host's to request, exactly as listed above: `antialias: false` (lines and circles
//! are feathered in the fragment shader), and `premultipliedAlpha: true` is REQUIRED
//! (the `SRC_ALPHA` over-blend writes premultiplied pixels, so the compositor must be
//! told, or every semi-transparent pixel double-darkens). The host forwards the two
//! context events to [`WebGL2Backend::on_context_lost`] (after calling
//! `preventDefault()` on the event so the context stays recoverable) and
//! [`WebGL2Backend::on_context_restored`]; the original's `onNeedsRedraw` /
//! `onContextFailed` callbacks are the return value of the latter, which the shell
//! acts on (repaint, or fall back to canvas2d).
//!
//! Index bookkeeping and the absent-slot convention are those of
//! [`Canvas2dBackend`](super::canvas2d_backend::Canvas2dBackend). One difference from that
//! port: a NaN visible range does not end the frame here (nor in the original, whose
//! `i1 < i0` check is false for NaN): the bar loops run zero times, and the backgrounds,
//! drawing slices and price lines still draw.
#![allow(clippy::too_many_arguments)]
// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::collections::HashMap;
use std::rc::Rc;

use crate::chart_types::BasePainting;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{
    Background, CandleBarColor, CandleKind, CandleStyle, Fill, IndicatorModel, LineLikeKind,
    LineLikeStyle, LineStyle, Ohlcv, PaneKind, PriceLine, SeriesData, SeriesPoint, SeriesSpec,
    SeriesSurface,
};
use crate::options::NativeBackend;
use crate::ports::chart_renderer::VelaTheme;
use crate::render::canvas::CanvasImage;
use crate::render::core::chart_config::{
    BASELINE_BOTTOM_LINE, BASELINE_FILL_ALPHA, BASELINE_FILL_ALPHA_FAR, BASELINE_TOP_LINE,
    COLUMNS_FILL_ALPHA, effective_candle_paint,
};
use crate::render::core::color::with_alpha;
use crate::render::core::{CoordinateSystem, PaneBounds, PaneNode, PriceScale, SceneGraph};
use crate::render::gl::{
    Buffer, Framebuffer, Gl, Program, Shader, Texture, UniformLocation, VertexArray, consts,
};

use super::candle_lod::{
    CandleTier, aggregate_candle_columns, candle_geometry, candle_tier, snap_y, wick_width,
};
use super::canvas2d_backend::theme_str;
use super::close_styles::{
    ClosePoint, close_runs, column_geometry, column_is_up, line_marker_radius,
    line_markers_visible, snap_stroke, step_vertices,
};
use super::gl::batch::Batch;
use super::gl::color::{Rgba, parse_color};
use super::render_backend::{BackendKind, RenderBackend};

const VERT_SRC: &str = "#version 300 es
layout(location=0) in vec2 aPos;
layout(location=1) in vec4 aColor;
layout(location=2) in vec2 aEdge;   // (signed perpendicular distance from line center, half-width)
uniform vec2 uRes;
out vec4 vColor;
out vec2 vEdge;
void main() {
    vec2 clip = vec2(aPos.x / uRes.x * 2.0 - 1.0, 1.0 - aPos.y / uRes.y * 2.0);
    gl_Position = vec4(clip, 0.0, 1.0);
    vColor = aColor;
    vEdge = aEdge;
}";

// Flat color for solid geometry (edgeHalf < 0); for line quads, feather the edge
// analytically: coverage falls off over ~1 device pixel (fwidth) around the half-width,
// independent of MSAA.
const FRAG_SRC: &str = "#version 300 es
precision highp float;
in vec4 vColor;
in vec2 vEdge;
out vec4 frag;
void main() {
    if (vEdge.y < 0.0) { frag = vColor; return; }
    float aa = fwidth(vEdge.x);
    float cov = clamp((vEdge.y - abs(vEdge.x)) / max(aa, 1e-4) + 0.5, 0.0, 1.0);
    frag = vec4(vColor.rgb, vColor.a * cov);
}";

// Fullscreen-quad shader for the glow post-process: a separable 5-tap Gaussian blur when
// uDir != 0, or a passthrough x intensity (the additive composite) when uDir == 0.
const SCREEN_VERT: &str = "#version 300 es
layout(location=0) in vec2 aPos;
out vec2 vUv;
void main() {
    vUv = aPos * 0.5 + 0.5;
    gl_Position = vec4(aPos, 0.0, 1.0);
}";

const SCREEN_FRAG: &str = "#version 300 es
precision mediump float;
in vec2 vUv;
uniform sampler2D uTex;
uniform vec2 uDir;        // blur direction (texel step); (0,0) = composite passthrough
uniform float uIntensity;
out vec4 frag;
void main() {
    if (uDir.x == 0.0 && uDir.y == 0.0) {
        frag = texture(uTex, vUv) * uIntensity;
        return;
    }
    vec2 o1 = uDir * 1.3846153846;
    vec2 o2 = uDir * 3.2307692308;
    vec4 sum = texture(uTex, vUv) * 0.2270270270;
    sum += texture(uTex, vUv + o1) * 0.3162162162;
    sum += texture(uTex, vUv - o1) * 0.3162162162;
    sum += texture(uTex, vUv + o2) * 0.0702702703;
    sum += texture(uTex, vUv - o2) * 0.0702702703;
    frag = sum;
}";

/// The dash pattern of a line style (`DASH` in the original).
fn dash(style: LineStyle) -> Option<[f64; 2]> {
    match style {
        LineStyle::Solid => None,
        LineStyle::Dashed => Some([6.0, 4.0]),
        LineStyle::Dotted => Some([2.0, 3.0]),
    }
}

/// Triangle-fan segment count for a round line join/cap of half-width `hw` (round
/// enough, not over-tessellated).
fn join_segments(hw: f64) -> usize {
    js_max(6.0, js_min(20.0, js_round(hw * 4.0))) as usize
}

/// JavaScript's `ToInt32`, for the `>>` the original applies to doubles.
fn to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    (x.trunc() % 4_294_967_296.0) as i64 as i32
}

/// `array[index]` with a JavaScript number index: `None` for a hole, a negative or
/// fractional index, or one past the end.
fn at<T>(items: &[T], index: f64) -> Option<&T> {
    if index >= 0.0 && index.fract() == 0.0 && index < items.len() as f64 {
        items.get(index as usize)
    } else {
        None
    }
}

/// The scale and pixel extent a series paints against (the original's `effPane`: the
/// pane with only its scale swapped for a merged indicator's own).
#[derive(Clone, Copy)]
struct Plane {
    scale: PriceScale,
    bounds: PaneBounds,
}

impl Plane {
    fn of(pane: &PaneNode) -> Self {
        Self {
            scale: pane.scale,
            bounds: pane.bounds,
        }
    }
}

fn y_of(coords: &CoordinateSystem, plane: &Plane, price: f64) -> f64 {
    coords.price_to_y(price, &plane.scale, &plane.bounds)
}

const ZERO: Rgba = [0.0, 0.0, 0.0, 0.0];

fn clamp01(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

fn lerp_color(a: Rgba, b: Rgba, t: f64) -> Rgba {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Fade toward transparent BLACK (rgb + alpha to 0) to match canvas2d's
/// `createLinearGradient(color to 'rgba(0,0,0,0)')` area fill.
fn fade_alpha(c: Rgba, t: f64) -> Rgba {
    let k = 1.0 - t;
    [c[0] * k, c[1] * k, c[2] * k, c[3] * k]
}

fn find_points<'a>(model: &'a IndicatorModel, series_id: &str) -> Option<&'a [SeriesPoint]> {
    model
        .series
        .iter()
        .find(|s| s.id == series_id)
        .and_then(SeriesSpec::points)
}

/// Every `barcolor()` entry of every indicator, by bar time (a later indicator
/// overrides an earlier one).
fn merge_bar_colors<'a>(models: impl Iterator<Item = &'a IndicatorModel>) -> HashMap<i64, String> {
    let mut map = HashMap::new();
    for m in models {
        for bc in &m.bar_colors {
            map.insert(bc.time, bc.color.clone());
        }
    }
    map
}

/// The backend mode a workspace cell uses (the original's `backendFor`): an explicit
/// backend is the host's word, the WebGL budget policy only decides `auto`. More cells
/// than `max_webgl_cells` (default 8) fall back to canvas2d, since browsers cap live
/// WebGL contexts per page.
pub fn backend_mode_for_cells(
    explicit: Option<NativeBackend>,
    cells: usize,
    max_webgl_cells: Option<usize>,
) -> NativeBackend {
    match explicit {
        Some(mode) if mode != NativeBackend::Auto => mode,
        _ => {
            if cells > max_webgl_cells.unwrap_or(8) {
                NativeBackend::Canvas2d
            } else {
                NativeBackend::Auto
            }
        }
    }
}

/// A compiled and linked pair of shaders.
fn compile(gl: &dyn Gl, kind: u32, src: &str) -> Option<Shader> {
    let sh = gl.create_shader(kind)?;
    gl.shader_source(sh, src);
    gl.compile_shader(sh);
    if !gl.get_shader_parameter(sh, consts::COMPILE_STATUS) {
        gl.delete_shader(sh);
        return None;
    }
    Some(sh)
}

pub struct WebGL2Backend {
    model_alpha: f64,
    candle_body_alpha: f64,
    candle_structure_alpha: f64,
    candle_body_scale: f64,
    /// Neon glow/bloom intensity for line series (0 = off). Set by the renderer.
    pub glow: f64,

    gl: Option<Rc<dyn Gl>>,
    /// The canvas's backing-store size in device pixels (`canvas.width`,
    /// `canvas.height`).
    backing: (f64, f64),
    program: Option<Program>,
    vao: Option<VertexArray>,
    vbo: Option<Buffer>,
    u_res: Option<UniformLocation>,
    batch: Batch,
    context_lost: bool,

    // glow/bloom post-process (lazy; only when glow > 0)
    screen_program: Option<Program>,
    u_tex: Option<UniformLocation>,
    u_dir: Option<UniformLocation>,
    u_intensity: Option<UniformLocation>,
    quad_vao: Option<VertexArray>,
    quad_vbo: Option<Buffer>,
    fbo_a: Option<Framebuffer>,
    tex_a: Option<Texture>,
    fbo_b: Option<Framebuffer>,
    tex_b: Option<Texture>,
    fbo_w: i32,
    fbo_h: i32,
    glow_batch: Batch,
    /// One texture per user-drawing interleave layer, keyed by its (stable, reused)
    /// canvas; re-uploaded every data frame since the layer repaints with the scene.
    /// Insertion-ordered, as the original's `Map`.
    slice_tex: Vec<(usize, Texture)>,
}

impl Default for WebGL2Backend {
    fn default() -> Self {
        Self::new()
    }
}

/// The identity of a slice canvas (what the original's `Map` keys on).
fn canvas_key(canvas: &CanvasImage) -> usize {
    Rc::as_ptr(canvas) as *const () as usize
}

impl WebGL2Backend {
    pub fn new() -> Self {
        Self {
            model_alpha: 1.0,
            candle_body_alpha: 1.0,
            candle_structure_alpha: 1.0,
            candle_body_scale: 1.0,
            glow: 0.0,
            gl: None,
            backing: (0.0, 0.0),
            program: None,
            vao: None,
            vbo: None,
            u_res: None,
            batch: Batch::new(),
            context_lost: false,
            screen_program: None,
            u_tex: None,
            u_dir: None,
            u_intensity: None,
            quad_vao: None,
            quad_vbo: None,
            fbo_a: None,
            tex_a: None,
            fbo_b: None,
            tex_b: None,
            fbo_w: 0,
            fbo_h: 0,
            glow_batch: Batch::new(),
            slice_tex: Vec::new(),
        }
    }

    /// Whether a usable WebGL2 context was acquired.
    pub fn ok(&self) -> bool {
        self.gl.is_some()
    }

    /// Bind the backend to its data canvas's GL context and the canvas's backing-store
    /// size in device pixels. When the program cannot be built the backend stays
    /// unmounted ([`ok`](Self::ok) is false) and the renderer uses canvas2d.
    pub fn mount(&mut self, gl: Rc<dyn Gl>, backing_width: f64, backing_height: f64) {
        self.backing = (backing_width, backing_height);
        self.gl = Some(gl);
        if !self.init_gl() {
            self.gl = None; // shaders/program failed: ok() = false, the renderer uses canvas2d
        }
    }

    /// The canvas was resized: its new backing-store size in device pixels.
    pub fn set_backing_size(&mut self, backing_width: f64, backing_height: f64) {
        self.backing = (backing_width, backing_height);
    }

    /// `webglcontextlost`: drawing pauses until the context is restored.
    pub fn on_context_lost(&mut self) {
        self.context_lost = true;
    }

    /// `webglcontextrestored`: rebuild the program and buffers. `true`: ready, repaint.
    /// `false`: the context cannot be reinitialized and the renderer should swap to
    /// canvas2d.
    pub fn on_context_restored(&mut self) -> bool {
        self.context_lost = false;
        self.init_gl()
    }

    /// (Re)compile the program and buffers. Returns false (and frees any partial GL
    /// objects) on failure, so both mount and a context restore can react.
    fn init_gl(&mut self) -> bool {
        let Some(gl) = self.gl.clone() else {
            return false;
        };
        let gl: &dyn Gl = &*gl;
        // Drop any stale glow handles (a context loss invalidated them) so they are rebuilt.
        self.screen_program = None;
        self.quad_vao = None;
        self.quad_vbo = None;
        self.fbo_a = None;
        self.fbo_b = None;
        self.tex_a = None;
        self.tex_b = None;
        self.fbo_w = 0;
        self.fbo_h = 0;
        self.slice_tex.clear(); // context-lost handles are invalid; textures re-upload on demand
        let vs = compile(gl, consts::VERTEX_SHADER, VERT_SRC);
        let fs = compile(gl, consts::FRAGMENT_SHADER, FRAG_SRC);
        let prog = gl.create_program();
        let (Some(vs), Some(fs), Some(prog)) = (vs, fs, prog) else {
            if let Some(vs) = vs {
                gl.delete_shader(vs);
            }
            if let Some(fs) = fs {
                gl.delete_shader(fs);
            }
            if let Some(prog) = prog {
                gl.delete_program(prog);
            }
            return false;
        };
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.link_program(prog);
        if !gl.get_program_parameter(prog, consts::LINK_STATUS) {
            gl.delete_program(prog);
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            return false;
        }
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        self.program = Some(prog);
        self.u_res = gl.get_uniform_location(prog, "uRes");
        self.vbo = gl.create_buffer();
        self.vao = gl.create_vertex_array();
        gl.bind_vertex_array(self.vao);
        gl.bind_buffer(consts::ARRAY_BUFFER, self.vbo);
        let stride = 8 * 4;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer(0, 2, consts::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer(1, 4, consts::FLOAT, false, stride, 2 * 4);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer(2, 2, consts::FLOAT, false, stride, 6 * 4);
        gl.bind_vertex_array(None);
        // Glow is optional: if its program/quad cannot be built, disable it but keep the
        // base backend working.
        if self.glow > 0.0 && !self.ensure_screen_pipeline() {
            self.glow = 0.0;
        }
        true
    }

    /// Compile the screen-quad program and fullscreen quad, shared by the glow
    /// post-process and the user-drawing interleave layers. Idempotent; FBOs (glow-only)
    /// are created lazily.
    fn ensure_screen_pipeline(&mut self) -> bool {
        if self.screen_program.is_some() && self.quad_vao.is_some() {
            return true;
        }
        let Some(gl) = self.gl.clone() else {
            return false;
        };
        let gl: &dyn Gl = &*gl;
        let vs = compile(gl, consts::VERTEX_SHADER, SCREEN_VERT);
        let fs = compile(gl, consts::FRAGMENT_SHADER, SCREEN_FRAG);
        let prog = gl.create_program();
        let (Some(vs), Some(fs), Some(prog)) = (vs, fs, prog) else {
            if let Some(vs) = vs {
                gl.delete_shader(vs);
            }
            if let Some(fs) = fs {
                gl.delete_shader(fs);
            }
            if let Some(prog) = prog {
                gl.delete_program(prog);
            }
            return false;
        };
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.link_program(prog);
        if !gl.get_program_parameter(prog, consts::LINK_STATUS) {
            gl.delete_program(prog);
            return false;
        }
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        self.screen_program = Some(prog);
        self.u_tex = gl.get_uniform_location(prog, "uTex");
        self.u_dir = gl.get_uniform_location(prog, "uDir");
        self.u_intensity = gl.get_uniform_location(prog, "uIntensity");
        self.quad_vbo = gl.create_buffer();
        self.quad_vao = gl.create_vertex_array();
        gl.bind_vertex_array(self.quad_vao);
        gl.bind_buffer(consts::ARRAY_BUFFER, self.quad_vbo);
        gl.buffer_data(
            consts::ARRAY_BUFFER,
            &[-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
            consts::STATIC_DRAW,
        );
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer(0, 2, consts::FLOAT, false, 0, 0);
        gl.bind_vertex_array(None);
        true
    }

    /// (Re)allocate the two half-res ping-pong FBOs to the given size.
    fn ensure_fbos(&mut self, gl: &dyn Gl, w: i32, h: i32) -> bool {
        if self.fbo_a.is_some() && self.fbo_w == w && self.fbo_h == h {
            return true;
        }
        self.delete_fbos(gl);
        let make = || -> (Option<Texture>, Option<Framebuffer>) {
            let tex = gl.create_texture();
            gl.bind_texture(consts::TEXTURE_2D, tex);
            gl.tex_image_2d_empty(
                consts::TEXTURE_2D,
                0,
                consts::RGBA,
                w,
                h,
                0,
                consts::RGBA,
                consts::UNSIGNED_BYTE,
            );
            gl.tex_parameteri(
                consts::TEXTURE_2D,
                consts::TEXTURE_MIN_FILTER,
                consts::LINEAR as i32,
            );
            gl.tex_parameteri(
                consts::TEXTURE_2D,
                consts::TEXTURE_MAG_FILTER,
                consts::LINEAR as i32,
            );
            gl.tex_parameteri(
                consts::TEXTURE_2D,
                consts::TEXTURE_WRAP_S,
                consts::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameteri(
                consts::TEXTURE_2D,
                consts::TEXTURE_WRAP_T,
                consts::CLAMP_TO_EDGE as i32,
            );
            let fbo = gl.create_framebuffer();
            gl.bind_framebuffer(consts::FRAMEBUFFER, fbo);
            gl.framebuffer_texture_2d(
                consts::FRAMEBUFFER,
                consts::COLOR_ATTACHMENT0,
                consts::TEXTURE_2D,
                tex,
                0,
            );
            (tex, fbo)
        };
        let a = make();
        let b = make();
        gl.bind_framebuffer(consts::FRAMEBUFFER, None);
        self.tex_a = a.0;
        self.fbo_a = a.1;
        self.tex_b = b.0;
        self.fbo_b = b.1;
        self.fbo_w = w;
        self.fbo_h = h;
        self.fbo_a.is_some() && self.fbo_b.is_some()
    }

    fn delete_fbos(&mut self, gl: &dyn Gl) {
        if let Some(f) = self.fbo_a {
            gl.delete_framebuffer(f);
        }
        if let Some(f) = self.fbo_b {
            gl.delete_framebuffer(f);
        }
        if let Some(t) = self.tex_a {
            gl.delete_texture(t);
        }
        if let Some(t) = self.tex_b {
            gl.delete_texture(t);
        }
        self.fbo_a = None;
        self.fbo_b = None;
        self.tex_a = None;
        self.tex_b = None;
        self.fbo_w = 0;
        self.fbo_h = 0;
    }

    /// Draw whatever geometry accumulated so far: called at each interleave point (a
    /// texture quad has to land BETWEEN geometry draws) and once at the pane's end. The
    /// quad pass swaps program/VAO/blend, so every flush rebinds the geometry pipeline.
    fn flush(&self, gl: &dyn Gl, b: &mut Batch, dpr: f64) {
        if b.vertex_count() == 0 {
            return;
        }
        gl.blend_func_separate(
            consts::SRC_ALPHA,
            consts::ONE_MINUS_SRC_ALPHA,
            consts::ONE,
            consts::ONE_MINUS_SRC_ALPHA,
        );
        gl.use_program(self.program);
        gl.bind_vertex_array(self.vao);
        gl.uniform2f(self.u_res, self.backing.0 / dpr, self.backing.1 / dpr);
        gl.bind_buffer(consts::ARRAY_BUFFER, self.vbo);
        gl.buffer_data(consts::ARRAY_BUFFER, b.view(), consts::DYNAMIC_DRAW);
        gl.draw_arrays(consts::TRIANGLES, 0, b.vertex_count() as i32);
        b.reset();
    }

    /// Composite the interleave layers whose `before_z` is at or below `z`.
    fn draw_slices_up_to(
        &mut self,
        gl: &dyn Gl,
        b: &mut Batch,
        dpr: f64,
        slices: &[crate::render::core::scene_graph::DrawingSlice],
        si: &mut usize,
        z: f64,
        live: &mut Vec<usize>,
    ) {
        while *si < slices.len() && slices[*si].before_z <= z {
            self.flush(gl, b, dpr);
            if self.draw_slice_texture(gl, &slices[*si].canvas) {
                live.push(canvas_key(&slices[*si].canvas));
            }
            *si += 1;
        }
    }

    /// Composite one user-drawing interleave layer: upload its canvas (premultiplied,
    /// y-flipped, since the framebuffer holds premultiplied pixels, see the module
    /// documentation) and draw it as a fullscreen quad through the screen pipeline. The
    /// pane scissor is already set, so it clips to the pane. Returns false when the screen
    /// pipeline cannot compile: the layer is skipped, not fatal.
    fn draw_slice_texture(&mut self, gl: &dyn Gl, canvas: &CanvasImage) -> bool {
        if !self.ensure_screen_pipeline() || self.screen_program.is_none() {
            return false;
        }
        let key = canvas_key(canvas);
        let existing = self
            .slice_tex
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, t)| *t);
        let tex = match existing {
            Some(t) => t,
            None => {
                let Some(t) = gl.create_texture() else {
                    return false;
                };
                self.slice_tex.push((key, t));
                gl.bind_texture(consts::TEXTURE_2D, Some(t));
                gl.tex_parameteri(
                    consts::TEXTURE_2D,
                    consts::TEXTURE_MIN_FILTER,
                    consts::LINEAR as i32,
                );
                gl.tex_parameteri(
                    consts::TEXTURE_2D,
                    consts::TEXTURE_MAG_FILTER,
                    consts::LINEAR as i32,
                );
                gl.tex_parameteri(
                    consts::TEXTURE_2D,
                    consts::TEXTURE_WRAP_S,
                    consts::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameteri(
                    consts::TEXTURE_2D,
                    consts::TEXTURE_WRAP_T,
                    consts::CLAMP_TO_EDGE as i32,
                );
                t
            }
        };
        gl.active_texture(consts::TEXTURE0);
        gl.bind_texture(consts::TEXTURE_2D, Some(tex));
        gl.pixel_storei(consts::UNPACK_FLIP_Y_WEBGL, true);
        gl.pixel_storei(consts::UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
        // Fresh pixels: the layer repaints with the scene.
        gl.tex_image_2d_canvas(
            consts::TEXTURE_2D,
            0,
            consts::RGBA,
            consts::RGBA,
            consts::UNSIGNED_BYTE,
            canvas,
        );
        gl.pixel_storei(consts::UNPACK_FLIP_Y_WEBGL, false);
        gl.pixel_storei(consts::UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
        gl.use_program(self.screen_program);
        gl.bind_vertex_array(self.quad_vao);
        gl.uniform1i(self.u_tex, 0);
        gl.uniform2f(self.u_dir, 0.0, 0.0); // passthrough (no blur)
        gl.uniform1f(self.u_intensity, 1.0);
        gl.blend_func_separate(
            consts::ONE,
            consts::ONE_MINUS_SRC_ALPHA,
            consts::ONE,
            consts::ONE_MINUS_SRC_ALPHA,
        );
        gl.draw_arrays(consts::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
        true
    }

    /// Neon bloom: re-render the line series into a half-res FBO, blur it (separable
    /// Gaussian, ping-pong), then additively composite the halo over the sharp scene.
    fn render_glow(
        &mut self,
        gl: &dyn Gl,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
    ) {
        let hw = js_max(1.0, (to_int32(self.backing.0) >> 1) as f64) as i32;
        let hh = js_max(1.0, (to_int32(self.backing.1) >> 1) as f64) as i32;
        if !self.ensure_fbos(gl, hw, hh) {
            return;
        }
        let dpr = coords.dpr();
        let data_w = coords.width();
        let (cw, ch) = self.backing;

        // glow-source pass: the neon lines into fboA (half-res)
        gl.bind_framebuffer(consts::FRAMEBUFFER, self.fbo_a);
        gl.viewport(0, 0, hw, hh);
        gl.disable(consts::SCISSOR_TEST);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(consts::COLOR_BUFFER_BIT);
        gl.enable(consts::BLEND);
        gl.blend_func_separate(
            consts::SRC_ALPHA,
            consts::ONE_MINUS_SRC_ALPHA,
            consts::ONE,
            consts::ONE_MINUS_SRC_ALPHA,
        );
        gl.use_program(self.program);
        gl.bind_vertex_array(self.vao);
        gl.uniform2f(self.u_res, cw / dpr, ch / dpr);
        gl.enable(consts::SCISSOR_TEST);
        let mut b = std::mem::take(&mut self.glow_batch);
        for pane in scene.ordered_panes() {
            b.reset();
            self.emit_glow_sources(&mut b, scene, pane, coords, i0, i1);
            if b.vertex_count() == 0 {
                continue;
            }
            let top_h = js_round(pane.bounds.top * dpr * 0.5);
            let bot_h = js_round((pane.bounds.top + pane.bounds.height) * dpr * 0.5);
            gl.scissor(
                0,
                (hh as f64 - bot_h) as i32,
                js_round(data_w * dpr * 0.5) as i32,
                (bot_h - top_h) as i32,
            );
            gl.bind_buffer(consts::ARRAY_BUFFER, self.vbo);
            gl.buffer_data(consts::ARRAY_BUFFER, b.view(), consts::DYNAMIC_DRAW);
            gl.draw_arrays(consts::TRIANGLES, 0, b.vertex_count() as i32);
        }
        self.glow_batch = b;
        gl.disable(consts::SCISSOR_TEST);

        // separable blur: fboA --H--> fboB --V--> fboA
        gl.use_program(self.screen_program);
        gl.bind_vertex_array(self.quad_vao);
        gl.uniform1i(self.u_tex, 0);
        gl.uniform1f(self.u_intensity, 1.0);
        gl.active_texture(consts::TEXTURE0);
        gl.disable(consts::BLEND);
        gl.bind_framebuffer(consts::FRAMEBUFFER, self.fbo_b);
        gl.bind_texture(consts::TEXTURE_2D, self.tex_a);
        gl.uniform2f(self.u_dir, 1.0 / f64::from(hw), 0.0);
        gl.draw_arrays(consts::TRIANGLE_STRIP, 0, 4);
        gl.bind_framebuffer(consts::FRAMEBUFFER, self.fbo_a);
        gl.bind_texture(consts::TEXTURE_2D, self.tex_b);
        gl.uniform2f(self.u_dir, 0.0, 1.0 / f64::from(hh));
        gl.draw_arrays(consts::TRIANGLE_STRIP, 0, 4);

        // composite the blurred glow additively over the screen
        gl.bind_framebuffer(consts::FRAMEBUFFER, None);
        gl.viewport(0, 0, cw as i32, ch as i32);
        gl.enable(consts::BLEND);
        gl.blend_func(consts::ONE, consts::ONE);
        gl.bind_texture(consts::TEXTURE_2D, self.tex_a);
        gl.uniform2f(self.u_dir, 0.0, 0.0); // composite (passthrough x intensity)
        gl.uniform1f(self.u_intensity, self.glow);
        gl.draw_arrays(consts::TRIANGLE_STRIP, 0, 4);
        gl.bind_texture(consts::TEXTURE_2D, None);
        gl.bind_vertex_array(None);
    }

    /// The "neon" elements that glow: line/area/step lines and point markers (not
    /// candles, fills or bars). Routing mirrors the main pass: own series per pane,
    /// force_overlay series on the price pane.
    fn emit_glow_sources(
        &self,
        b: &mut Batch,
        scene: &SceneGraph,
        pane: &PaneNode,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
    ) {
        let plane = Plane::of(pane);
        let emit_one = |b: &mut Batch, s: &SeriesSpec, off: f64| {
            if !s.shown_on(SeriesSurface::Pane) {
                return;
            }
            let SeriesData::LineLike {
                kind,
                points,
                style,
            } = &s.data
            else {
                return;
            };
            match kind {
                LineLikeKind::Histogram | LineLikeKind::Columns => {}
                LineLikeKind::Circles | LineLikeKind::Cross => {
                    self.emit_point_markers(b, *kind, points, style, &plane, coords, i0, i1, off);
                }
                _ => self.emit_polyline(
                    b,
                    points,
                    style,
                    &plane,
                    coords,
                    i0,
                    i1,
                    *kind == LineLikeKind::Step,
                    off,
                ),
            }
        };
        for m in scene.indicators_for_pane(&pane.id) {
            let off = scene.offset_of(&m.id) as f64;
            for s in &m.series {
                if !s.overlay {
                    emit_one(b, s, off);
                }
            }
        }
        if pane.kind == PaneKind::Price {
            for m in scene.indicators.values() {
                let off = scene.offset_of(&m.id) as f64;
                for s in &m.series {
                    if s.overlay {
                        emit_one(b, s, off);
                    }
                }
            }
        }
    }

    // ── geometry emit (mirrors Canvas2dBackend, in CSS-px space) ──

    fn emit_background(
        &self,
        b: &mut Batch,
        bg: &Background,
        plane: &Plane,
        coords: &CoordinateSystem,
    ) {
        let x1 = coords.time_to_x(bg.from as f64);
        let x2 = coords.time_to_x(bg.to as f64);
        if x2 < 0.0 || x1 > coords.width() || x2 <= x1 {
            return;
        }
        b.rect(
            x1,
            plane.bounds.top,
            x2 - x1,
            plane.bounds.height,
            parse_color(Some(&bg.color)),
        );
    }

    fn emit_fill(
        &self,
        b: &mut Batch,
        model: &IndicatorModel,
        fill: &Fill,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let (Some(from), Some(to)) = (
            find_points(model, &fill.from_series_id),
            find_points(model, &fill.to_series_id),
        ) else {
            return;
        };
        let mut i = js_max(1.0, i0);
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let value_at = |points: &[SeriesPoint], idx: f64| at(points, idx).and_then(|p| p.value);
            let (Some(a0), Some(b0), Some(a1), Some(b1)) = (
                value_at(from, cur - 1.0 - off),
                value_at(to, cur - 1.0 - off),
                value_at(from, cur - off),
                value_at(to, cur - off),
            ) else {
                continue;
            };
            let x_prev = coords.logical_to_x(cur - 1.0);
            let x_cur = coords.logical_to_x(cur);
            let t_prev = y_of(coords, plane, a0);
            let b_prev = y_of(coords, plane, b0);
            let t_cur = y_of(coords, plane, a1);
            let b_cur = y_of(coords, plane, b1);
            // Style by the LEFT/head column (matches FillPrimitive's run-head convention).
            let grad = fill
                .gradient
                .as_deref()
                .and_then(|g| at(g, cur - 1.0 - off))
                .and_then(Option::as_ref);
            if let Some(grad) = grad {
                let y_top = y_of(coords, plane, grad.top_value);
                let y_bot0 = y_of(coords, plane, grad.bottom_value);
                // match canvas2d's degenerate nudge
                let y_bot = if y_bot0 == y_top { y_top + 1.0 } else { y_bot0 };
                let tc = parse_color(Some(&grad.top_color));
                let bc = parse_color(Some(&grad.bottom_color));
                let span = y_bot - y_top;
                let at_y = |y: f64| lerp_color(tc, bc, clamp01((y - y_top) / span));
                // quad corners: TL(xPrev,tPrev) TR(xCur,tCur) BR(xCur,bCur) BL(xPrev,bPrev)
                b.quad4(
                    x_prev,
                    t_prev,
                    at_y(t_prev),
                    x_cur,
                    t_cur,
                    at_y(t_cur),
                    x_cur,
                    b_cur,
                    at_y(b_cur),
                    x_prev,
                    b_prev,
                    at_y(b_prev),
                );
            } else {
                let color = match &fill.colors {
                    Some(colors) => at(colors, cur - 1.0 - off).and_then(Option::as_ref),
                    None => fill.color.as_ref(),
                };
                let Some(color) = color.filter(|c| !c.is_empty()) else {
                    continue;
                };
                b.quad(
                    x_prev,
                    t_prev,
                    x_cur,
                    t_cur,
                    x_cur,
                    b_cur,
                    x_prev,
                    b_prev,
                    parse_color(Some(color)),
                );
            }
        }
    }

    // ── base price series (chart type), mirrors Canvas2dBackend.draw_price_series ──

    fn emit_price_series(
        &self,
        b: &mut Batch,
        scene: &SceneGraph,
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        theme: &VelaTheme,
        bar_colors: &HashMap<i64, String>,
        data_w: f64,
    ) {
        if scene.base_painting == BasePainting::None {
            return; // a plugin style fully replaces the price series
        }
        let bars = &scene.bars;
        let st = &scene.style;
        let up_color = theme_str(theme, "upColor");
        let down_color = theme_str(theme, "downColor");
        match scene.price_style.as_str() {
            "bars" => {
                let up = st.bars.up_color.as_deref().unwrap_or(up_color);
                let down = st.bars.down_color.as_deref().unwrap_or(down_color);
                self.emit_bars(b, bars, i0, i1, coords, plane, up, down, bar_colors);
            }
            "line" => {
                let color = st.line.color.as_deref().unwrap_or(up_color);
                self.emit_price_line(
                    b,
                    bars,
                    i0,
                    i1,
                    coords,
                    plane,
                    parse_color(Some(color)),
                    st.line.width,
                );
            }
            "linemarkers" => {
                let c = parse_color(Some(st.line.color.as_deref().unwrap_or(up_color)));
                self.emit_price_line(b, bars, i0, i1, coords, plane, c, st.line.width);
                self.emit_line_markers(b, bars, i0, i1, coords, plane, c, st.line.width);
            }
            "stepline" => {
                let color = st.line.color.as_deref().unwrap_or(up_color);
                self.emit_step_line(
                    b,
                    bars,
                    i0,
                    i1,
                    coords,
                    plane,
                    parse_color(Some(color)),
                    st.line.width,
                );
            }
            "area" => {
                let line_color = st.area.line_color.as_deref().unwrap_or(up_color);
                self.emit_price_area(
                    b,
                    bars,
                    i0,
                    i1,
                    coords,
                    plane,
                    parse_color(Some(st.area.top_color.as_deref().unwrap_or(line_color))),
                    parse_color(Some(
                        st.area.bottom_color.as_deref().unwrap_or("rgba(0,0,0,0)"),
                    )),
                );
                self.emit_price_line(
                    b,
                    bars,
                    i0,
                    i1,
                    coords,
                    plane,
                    parse_color(Some(line_color)),
                    st.area.width,
                );
            }
            "baseline" => {
                self.emit_baseline(b, bars, i0, i1, coords, plane, scene, theme, data_w);
            }
            "columns" => {
                let up = st
                    .columns
                    .up_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(up_color, COLUMNS_FILL_ALPHA));
                let down = st
                    .columns
                    .down_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(down_color, COLUMNS_FILL_ALPHA));
                self.emit_columns(b, bars, i0, i1, coords, plane, &up, &down, bar_colors);
            }
            // 'candles' + candle-drawn styles (a plugin style may fade candles to reveal
            // its order-flow layer) and 'heikinashi' (the bars themselves arrive already
            // transformed)
            _ => self.emit_candles(b, scene, bars, i0, i1, coords, plane, theme, bar_colors),
        }
    }

    /// OHLC bars: high-low stick + left open tick + right close tick (no body).
    fn emit_bars(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        up: &str,
        down: &str,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let tier = candle_tier(spacing);
        let color_of = |bar: &Ohlcv| -> String {
            bar_colors
                .get(&bar.time)
                .cloned()
                .unwrap_or_else(|| if bar.close >= bar.open { up } else { down }.to_string())
        };
        if tier == CandleTier::Aggregate {
            self.emit_candles_aggregated(b, bars, i0, i1, coords, plane, &color_of);
            return;
        }
        let tick_w = js_max(1.0, js_round(spacing * 0.35));
        let draw_ticks = tier == CandleTier::Full;
        for i in i0..=i1 {
            let Some(bar) = bars.get(i) else { continue };
            if bar.high <= bar.low {
                continue;
            }
            let c = parse_color(Some(&color_of(bar)));
            let x = js_round(coords.logical_to_x(i as f64));
            let h_y = y_of(coords, plane, bar.high);
            let l_y = y_of(coords, plane, bar.low);
            b.rect(x - 0.5, h_y, 1.0, l_y - h_y, c);
            if draw_ticks {
                let o_y = y_of(coords, plane, bar.open);
                let c_y = y_of(coords, plane, bar.close);
                b.rect(x - tick_w, o_y - 0.5, tick_w, 1.0, c);
                b.rect(x, c_y - 0.5, tick_w, 1.0, c);
            }
        }
    }

    /// Close-price line (also the top stroke of the area style).
    fn emit_price_line(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        c: Rgba,
        width: f64,
    ) {
        let hw = js_max(1.0, width) / 2.0;
        let segs = join_segments(hw);
        let mut have = false;
        let (mut px, mut py) = (0.0, 0.0);
        for i in i0.saturating_sub(1)..=i1 {
            let Some(bar) = bars.get(i) else {
                have = false;
                continue;
            };
            let x = coords.logical_to_x(i as f64);
            let y = y_of(coords, plane, bar.close);
            if have {
                b.seg(px, py, x, y, width, c);
            }
            // round join at each vertex (round cap at run ends): no faceted kinks
            b.circle(x, y, hw, c, segs);
            px = x;
            py = y;
            have = true;
        }
    }

    /// Line with markers (mirrors Canvas2dBackend.draw_line_markers): a dot on each bar's
    /// close once bars are spaced enough to tell the dots apart.
    fn emit_line_markers(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        c: Rgba,
        width: f64,
    ) {
        let r = line_marker_radius(width);
        if !line_markers_visible(coords.px_per_bar(), r) {
            return;
        }
        let segs = js_max(16.0, f64::ceil(r * 2.0)) as usize;
        for i in i0.saturating_sub(1)..=i1 {
            let Some(bar) = bars.get(i) else { continue };
            b.circle(
                coords.logical_to_x(i as f64),
                y_of(coords, plane, bar.close),
                r,
                c,
                segs,
            );
        }
    }

    /// Step line (mirrors Canvas2dBackend.draw_step_line): the snapped step vertices as
    /// axis-aligned rects, each run square-capped at both ends. Every rect owns the
    /// corner square at its END (the first one also at its start), so no pixel is covered
    /// twice and a translucent line keeps one even tone through its corners.
    fn emit_step_line(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        c: Rgba,
        width: f64,
    ) {
        let dpr = coords.dpr();
        let hw = js_max(1.0, width) / 2.0;
        let runs = close_runs(i0.saturating_sub(1), i1, &|i| {
            bars.get(i).map(|bar| ClosePoint {
                x: snap_stroke(coords.logical_to_x(i as f64), width, dpr),
                y: snap_stroke(y_of(coords, plane, bar.close), width, dpr),
            })
        });
        for run in &runs {
            let vs = step_vertices(run);
            for k in 1..vs.len() {
                let a = vs[k - 1];
                let z = vs[k];
                if a.y == z.y {
                    // Horizontal, always left to right.
                    let x0 = if k == 1 { a.x - hw } else { a.x + hw };
                    let x1 = z.x + hw;
                    if x1 > x0 {
                        b.rect(x0, a.y - hw, x1 - x0, hw * 2.0, c);
                    }
                } else {
                    // Vertical: never a run's first segment, so its start corner is the
                    // previous horizontal's.
                    let y0 = if z.y > a.y { a.y + hw } else { z.y - hw };
                    let y1 = if z.y > a.y { z.y + hw } else { a.y - hw };
                    if y1 > y0 {
                        b.rect(a.x - hw, y0, hw * 2.0, y1 - y0, c);
                    }
                }
            }
        }
    }

    /// Columns (mirrors Canvas2dBackend.draw_columns): one per bar from the pane's bottom
    /// edge up to its close, up/down by the close against the previous close.
    fn emit_columns(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        up: &str,
        down: &str,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let dpr = coords.dpr();
        let bottom = snap_y(plane.bounds.top + plane.bounds.height, dpr);
        let up_c = parse_color(Some(up));
        let down_c = parse_color(Some(down));
        for i in i0..=i1 {
            let Some(bar) = bars.get(i) else { continue };
            let top = snap_y(y_of(coords, plane, bar.close), dpr);
            if !(top < bottom) {
                continue;
            }
            let g = column_geometry(coords.logical_to_x(i as f64), spacing, dpr);
            let color = match bar_colors.get(&bar.time).filter(|c| !c.is_empty()) {
                Some(bc) => parse_color(Some(bc)),
                None if column_is_up(bars, i) => up_c,
                None => down_c,
            };
            b.rect(g.left, top, g.width, bottom - top, color);
        }
    }

    /// Filled area under the close line: per-bar quads gradient `top` (at the line) to
    /// `bottom` (at the pane floor). Defaults fade the line color out (bottom
    /// transparent).
    fn emit_price_area(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        top: Rgba,
        bottom: Rgba,
    ) {
        let base_y = plane.bounds.top + plane.bounds.height;
        let mut run: Vec<(f64, f64)> = Vec::new();
        let mut min_y = f64::INFINITY;
        let flush = |b: &mut Batch, run: &mut Vec<(f64, f64)>, min_y: &mut f64| {
            if run.len() >= 2 {
                let span = base_y - *min_y;
                let at_y = |y: f64| {
                    lerp_color(
                        top,
                        bottom,
                        if span < 1e-6 {
                            0.0
                        } else {
                            clamp01((y - *min_y) / span)
                        },
                    )
                };
                for k in 1..run.len() {
                    let a = run[k - 1];
                    let c = run[k];
                    b.quad4(
                        a.0,
                        a.1,
                        at_y(a.1),
                        c.0,
                        c.1,
                        at_y(c.1),
                        c.0,
                        base_y,
                        bottom,
                        a.0,
                        base_y,
                        bottom,
                    );
                }
            }
            run.clear();
            *min_y = f64::INFINITY;
        };
        for i in i0.saturating_sub(1)..=i1 {
            let Some(bar) = bars.get(i) else {
                flush(b, &mut run, &mut min_y);
                continue;
            };
            let x = coords.logical_to_x(i as f64);
            let y = y_of(coords, plane, bar.close);
            run.push((x, y));
            if y < min_y {
                min_y = y;
            }
        }
        flush(b, &mut run, &mut min_y);
    }

    /// Baseline style: per-segment fill quads down to the baseline, up color above /
    /// down color below. Each area is a two-stop gradient: the near-line fill far from
    /// the baseline blending to the fainter near-baseline fill at it. A dashed baseline
    /// marker + the per-segment line on top.
    fn emit_baseline(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        scene: &SceneGraph,
        theme: &VelaTheme,
        data_w: f64,
    ) {
        let baseline = scene.baseline_price_for(&plane.scale);
        let base_y = y_of(coords, plane, baseline);
        let bs = &scene.style.baseline;
        let top_line_str = bs.top_line_color.as_deref().unwrap_or(BASELINE_TOP_LINE);
        let bottom_line_str = bs
            .bottom_line_color
            .as_deref()
            .unwrap_or(BASELINE_BOTTOM_LINE);
        let top_line = parse_color(Some(top_line_str));
        let bottom_line = parse_color(Some(bottom_line_str));
        let top_fill = parse_color(Some(
            &bs.top_fill_color
                .clone()
                .unwrap_or_else(|| with_alpha(top_line_str, BASELINE_FILL_ALPHA)),
        ));
        let top_fill2 =
            parse_color(Some(&bs.top_fill_color2.clone().unwrap_or_else(|| {
                with_alpha(top_line_str, BASELINE_FILL_ALPHA_FAR)
            })));
        let bottom_fill =
            parse_color(Some(&bs.bottom_fill_color.clone().unwrap_or_else(|| {
                with_alpha(bottom_line_str, BASELINE_FILL_ALPHA)
            })));
        let bottom_fill2 =
            parse_color(Some(&bs.bottom_fill_color2.clone().unwrap_or_else(|| {
                with_alpha(bottom_line_str, BASELINE_FILL_ALPHA_FAR)
            })));
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for i in i0.saturating_sub(1)..=i1 {
            let Some(bar) = bars.get(i) else { continue };
            pts.push((
                coords.logical_to_x(i as f64),
                y_of(coords, plane, bar.close),
            ));
        }
        if pts.len() < 2 {
            return;
        }
        let top = plane.bounds.top;
        let bottom = plane.bounds.top + plane.bounds.height;
        // The near-baseline fill for a vertex's side (used at the baseline edge of the quad).
        let far_fill = |y: f64| if y <= base_y { top_fill2 } else { bottom_fill2 };
        // Fill at a line vertex: blend the near-baseline to near-line fill by how far the
        // close sits from the baseline (matches canvas2d's clipped two-stop gradient).
        let side_fill = |y: f64| {
            let above = y <= base_y;
            let region = if above { base_y - top } else { bottom - base_y };
            let t = if region > 0.0 {
                clamp01((y - base_y).abs() / region)
            } else {
                0.0
            };
            lerp_color(
                if above { top_fill2 } else { bottom_fill2 },
                if above { top_fill } else { bottom_fill },
                t,
            )
        };
        for k in 1..pts.len() {
            let a = pts[k - 1];
            let c = pts[k];
            // Split the fill AT the baseline crossing (mirrors the stroke split below): a
            // single quad down to the baseline would straddle both sides and bleed one
            // side's fill color onto the other. Each half is a triangle wholly on its side.
            if (a.1 <= base_y) != (c.1 <= base_y) && c.1 != a.1 {
                let t = (base_y - a.1) / (c.1 - a.1);
                let x_cross = a.0 + (c.0 - a.0) * t;
                b.tri(
                    a.0,
                    a.1,
                    side_fill(a.1),
                    x_cross,
                    base_y,
                    far_fill(a.1),
                    a.0,
                    base_y,
                    far_fill(a.1),
                );
                b.tri(
                    x_cross,
                    base_y,
                    far_fill(c.1),
                    c.0,
                    c.1,
                    side_fill(c.1),
                    c.0,
                    base_y,
                    far_fill(c.1),
                );
            } else {
                b.quad4(
                    a.0,
                    a.1,
                    side_fill(a.1),
                    c.0,
                    c.1,
                    side_fill(c.1),
                    c.0,
                    base_y,
                    far_fill(c.1),
                    a.0,
                    base_y,
                    far_fill(a.1),
                );
            }
        }
        let border = scene
            .style
            .border_color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        b.dashed_seg(
            0.0,
            base_y,
            data_w,
            base_y,
            1.0,
            parse_color(Some(border)),
            dash(LineStyle::Dashed),
        );
        for k in 1..pts.len() {
            let a = pts[k - 1];
            let c = pts[k];
            let a_up = a.1 <= base_y;
            let c_up = c.1 <= base_y;
            // Split the segment AT the baseline so each side is colored correctly: a
            // segment must never be the up color below the baseline (or vice versa).
            if a_up != c_up && c.1 != a.1 {
                let t = (base_y - a.1) / (c.1 - a.1);
                let x_cross = a.0 + (c.0 - a.0) * t;
                b.seg(
                    a.0,
                    a.1,
                    x_cross,
                    base_y,
                    bs.width,
                    if a_up { top_line } else { bottom_line },
                );
                b.seg(
                    x_cross,
                    base_y,
                    c.0,
                    c.1,
                    bs.width,
                    if c_up { top_line } else { bottom_line },
                );
            } else {
                b.seg(
                    a.0,
                    a.1,
                    c.0,
                    c.1,
                    bs.width,
                    if a_up { top_line } else { bottom_line },
                );
            }
        }
    }

    fn emit_candles(
        &self,
        b: &mut Batch,
        scene: &SceneGraph,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        theme: &VelaTheme,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let tier = candle_tier(spacing);
        // A candle-based plugin style paints with its OWN cosmetics (unset keys inherit
        // the shared candles block); built-ins pass through untouched.
        let paint = effective_candle_paint(
            &scene.style.candle,
            scene.candle_override.as_ref(),
            theme_str(theme, "upColor"),
            theme_str(theme, "downColor"),
        );
        let cs = &paint.candle;
        if tier == CandleTier::Aggregate {
            // The stick IS the candle, so a bar's paint resolves as in the stick-only tier
            // below: the wick color setting wins, then barcolor(), then the direction
            // color.
            self.emit_candles_aggregated(b, bars, i0, i1, coords, plane, &|bar: &Ohlcv| {
                let is_up = bar.close >= bar.open;
                let wick = if is_up {
                    &cs.wick_up_color
                } else {
                    &cs.wick_down_color
                };
                wick.clone()
                    .or_else(|| bar_colors.get(&bar.time).cloned())
                    .unwrap_or_else(|| if is_up { &paint.up } else { &paint.down }.clone())
            });
            return;
        }
        let draw_body = tier == CandleTier::Full;
        // When a fading style drops the body below the structure, draw a body outline even
        // if no border is configured, so the candle keeps a visible (hollow) skeleton.
        let fading = self.candle_structure_alpha > self.candle_body_alpha + 0.001;
        let dpr = coords.dpr();
        for i in i0..=i1 {
            let Some(bar) = bars.get(i) else { continue };
            if bar.high <= bar.low {
                continue; // skip zero-range candles (e.g. the intro reveal's un-started state)
            }
            // Wick + body snapped to the device grid as one unit, so the wick is always
            // dead-center in the body and the gap between candles stays uniform.
            let g = candle_geometry(
                coords.logical_to_x(i as f64),
                spacing,
                dpr,
                self.candle_body_scale,
            );
            let is_up = bar.close >= bar.open;
            let dir = if is_up { &paint.up } else { &paint.down };
            let body_color_str = bar_colors.get(&bar.time).unwrap_or(dir);
            let c = parse_color(Some(body_color_str));
            // Body geometry up front so the wick can be clipped to it when the body is
            // hollow.
            let mut body_top = 0.0;
            let mut body_h = 0.0;
            if draw_body {
                let o_y = y_of(coords, plane, bar.open);
                let c_y = y_of(coords, plane, bar.close);
                // Both edges snapped to the device grid so the horizontal edges rasterize
                // crisp (no blended rim); a doji keeps a visible 1-device-px body.
                body_top = snap_y(js_min(o_y, c_y), dpr);
                body_h = js_max(1.0 / dpr, snap_y(js_max(o_y, c_y), dpr) - body_top);
            }
            if cs.wick_visible {
                let h_y = snap_y(y_of(coords, plane, bar.high), dpr);
                let l_y = snap_y(y_of(coords, plane, bar.low), dpr);
                b.alpha = self.candle_structure_alpha;
                // barcolor() recolors only the BODY (TV semantics): the wick keeps the
                // direction color while a body is drawn. At stick-only zoom the stick IS
                // the candle, so it keeps the barcolor tint.
                let wick_setting = if is_up {
                    &cs.wick_up_color
                } else {
                    &cs.wick_down_color
                };
                let w_col = parse_color(Some(wick_setting.as_ref().unwrap_or(if draw_body {
                    dir
                } else {
                    body_color_str
                })));
                if draw_body {
                    // Draw the wick only outside the body, so it never shows through it,
                    // including hollow bodies and semi-transparent fills.
                    b.rect(g.wick_x, h_y, g.wick_w, js_max(0.0, body_top - h_y), w_col);
                    b.rect(
                        g.wick_x,
                        body_top + body_h,
                        g.wick_w,
                        js_max(0.0, l_y - (body_top + body_h)),
                        w_col,
                    );
                } else {
                    b.rect(g.wick_x, h_y, g.wick_w, l_y - h_y, w_col);
                }
            }
            if draw_body {
                if cs.body_visible {
                    b.alpha = self.candle_body_alpha;
                    b.rect(g.body_x, body_top, g.body_w, body_h, c);
                }
                // The border strictly follows its visibility setting: barcolor() never
                // forces one. An unconfigured border color inherits the body color, so a
                // barcolored body gets a matching tinted border, not a direction-colored
                // outline.
                if cs.border_visible || (fading && cs.body_visible) {
                    b.alpha = self.candle_structure_alpha;
                    let bord = if cs.border_visible {
                        let configured = if is_up {
                            &cs.border_up_color
                        } else {
                            &cs.border_down_color
                        };
                        parse_color(Some(configured.as_ref().unwrap_or(body_color_str)))
                    } else {
                        c
                    };
                    // Inset by half the stroke so the border stays inside the body's
                    // snapped footprint (mirrors the canvas2d backend).
                    let bw = js_max(0.0, g.body_w - 1.0);
                    let bh = js_max(0.0, body_h - 1.0);
                    b.rect_stroke(g.body_x + 0.5, body_top + 0.5, bw, bh, 1.0, bord);
                }
            }
        }
    }

    /// Sub-pixel LOD (mirrors Canvas2dBackend.draw_candles_aggregated): bars sharing a
    /// rounded pixel column collapse into high-low sticks, one per contiguous SAME-COLOR
    /// coverage run (see `aggregate_candle_columns`), so a price gap inside the column
    /// stays a void and every bar's extent keeps its own color. Draw cost stays bounded
    /// by screen width, not bar count. `color_of` is the calling style's per-bar paint
    /// rule (candles and OHLC bars resolve theirs differently).
    fn emit_candles_aggregated(
        &self,
        b: &mut Batch,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        color_of: &dyn Fn(&Ohlcv) -> String,
    ) {
        let price_y = |price: f64| y_of(coords, plane, price);
        for s in aggregate_candle_columns(
            bars,
            i0,
            i1,
            &|i| coords.logical_to_x(i as f64),
            &price_y,
            color_of,
        ) {
            let c = parse_color(Some(&s.color));
            let h_y = price_y(s.hi);
            b.rect(s.x - 0.5, h_y, 1.0, price_y(s.lo) - h_y, c);
        }
    }

    fn emit_series(
        &self,
        b: &mut Batch,
        spec: &SeriesSpec,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        theme: &VelaTheme,
        off: f64,
    ) {
        // Off-pane series (legend/data-window-only readouts, hidden fill anchors) are kept
        // in the model for fills and readouts but never painted here.
        if !spec.shown_on(SeriesSurface::Pane) {
            return;
        }
        match &spec.data {
            SeriesData::Candle {
                kind,
                bars,
                style,
                bar_colors,
            } => self.emit_plot_candles(
                b,
                *kind,
                bars,
                style.as_ref(),
                bar_colors.as_deref(),
                plane,
                coords,
                i0,
                i1,
                theme,
                off,
            ),
            SeriesData::Markers { .. } => {}
            SeriesData::LineLike {
                kind,
                points,
                style,
            } => match kind {
                LineLikeKind::Histogram | LineLikeKind::Columns => {
                    self.emit_histogram(b, *kind, points, style, plane, coords, i0, i1, off);
                }
                LineLikeKind::Area => {
                    self.emit_area(b, points, style, plane, coords, i0, i1, off);
                    self.emit_polyline(b, points, style, plane, coords, i0, i1, false, off);
                }
                LineLikeKind::Circles | LineLikeKind::Cross => {
                    self.emit_point_markers(b, *kind, points, style, plane, coords, i0, i1, off);
                }
                LineLikeKind::Step => {
                    self.emit_polyline(b, points, style, plane, coords, i0, i1, true, off);
                }
                LineLikeKind::Line => {
                    self.emit_polyline(b, points, style, plane, coords, i0, i1, false, off);
                }
            },
        }
    }

    fn emit_polyline(
        &self,
        b: &mut Batch,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        step: bool,
        off: f64,
    ) {
        let w = js_max(1.0, style.width);
        let hw = w / 2.0;
        let pattern = dash(style.line_style);
        // Round joins/caps for solid straight lines only: the butt-capped segments
        // otherwise leave a hard notch/kink at every vertex (visible when zoomed in).
        // Dashed + step keep their corners.
        let rounded = pattern.is_none() && !step;
        let segs = join_segments(hw);
        let mut have = false;
        let mut i = js_max(1.0, i0 - 1.0);
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let (Some(p), Some(pv)) = (at(points, cur - off), at(points, cur - 1.0 - off)) else {
                have = false;
                continue;
            };
            let (Some(value), Some(prev_value)) = (p.value, pv.value) else {
                have = false;
                continue;
            };
            let x = coords.logical_to_x(cur);
            let y = y_of(coords, plane, value);
            let px = coords.logical_to_x(cur - 1.0);
            let py = y_of(coords, plane, prev_value);
            let c = parse_color(Some(p.color.as_deref().unwrap_or(&style.color)));
            if step {
                b.dashed_seg(px, py, x, py, w, c, pattern);
                b.dashed_seg(x, py, x, y, w, c, pattern);
            } else {
                b.dashed_seg(px, py, x, y, w, c, pattern);
            }
            if rounded {
                if !have {
                    // cap at a run's start
                    b.circle(
                        px,
                        py,
                        hw,
                        parse_color(Some(pv.color.as_deref().unwrap_or(&style.color))),
                        segs,
                    );
                }
                b.circle(x, y, hw, c, segs); // round join at this vertex (cap at a run's end)
            }
            have = true;
        }
    }

    fn emit_area(
        &self,
        b: &mut Batch,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let base_y = plane.bounds.top + plane.bounds.height;
        let base = parse_color(Some(&style.color));
        // Per contiguous run: a baseline-to-line band with a top-color to transparent fade
        // (gradient via per-vertex alpha; matches the canvas2d minY-to-baseY gradient).
        let mut run: Vec<(f64, f64)> = Vec::new();
        let mut min_y = f64::INFINITY;
        let flush = |b: &mut Batch, run: &mut Vec<(f64, f64)>, min_y: &mut f64| {
            if run.is_empty() {
                *min_y = f64::INFINITY;
                return;
            }
            let span = base_y - *min_y;
            let at_y = |y: f64| {
                fade_alpha(
                    base,
                    if span < 1e-6 {
                        0.0
                    } else {
                        clamp01((y - *min_y) / span)
                    },
                )
            };
            for k in 1..run.len() {
                let a = run[k - 1];
                let c = run[k];
                // quad: line-top (a, c) down to baseline: top verts faded by y, base verts
                // transparent.
                b.quad4(
                    a.0,
                    a.1,
                    at_y(a.1),
                    c.0,
                    c.1,
                    at_y(c.1),
                    c.0,
                    base_y,
                    ZERO,
                    a.0,
                    base_y,
                    ZERO,
                );
            }
            run.clear();
            *min_y = f64::INFINITY;
        };
        let mut i = i0;
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let Some(value) = at(points, cur - off).and_then(|p| p.value) else {
                flush(b, &mut run, &mut min_y);
                continue;
            };
            let x = coords.logical_to_x(cur);
            let y = y_of(coords, plane, value);
            run.push((x, y));
            if y < min_y {
                min_y = y;
            }
        }
        flush(b, &mut run, &mut min_y);
    }

    fn emit_histogram(
        &self,
        b: &mut Batch,
        kind: LineLikeKind,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let spacing = coords.body_spacing();
        let w = js_max(
            1.0,
            (if kind == LineLikeKind::Columns {
                0.7
            } else {
                0.8
            }) * spacing,
        );
        let half = w / 2.0;
        let base = style.base.unwrap_or(0.0);
        let base_y = y_of(coords, plane, base);
        let mut i = i0;
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let Some(p) = at(points, cur - off) else {
                continue;
            };
            let Some(value) = p.value else { continue };
            let x = coords.logical_to_x(cur);
            let y = y_of(coords, plane, value);
            b.rect(
                x - half,
                js_min(base_y, y),
                w,
                js_max(1.0, (y - base_y).abs()),
                parse_color(Some(p.color.as_deref().unwrap_or(&style.color))),
            );
        }
    }

    fn emit_point_markers(
        &self,
        b: &mut Batch,
        kind: LineLikeKind,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let r = js_max(1.5, style.width);
        let mut i = i0;
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let Some(p) = at(points, cur - off) else {
                continue;
            };
            let Some(value) = p.value else { continue };
            let x = coords.logical_to_x(cur);
            let y = y_of(coords, plane, value);
            let c = parse_color(Some(p.color.as_deref().unwrap_or(&style.color)));
            if kind == LineLikeKind::Cross {
                b.seg(x - r, y, x + r, y, 1.0, c);
                b.seg(x, y - r, x, y + r, 1.0, c);
            } else {
                // smoother at larger radii
                b.circle(x, y, r, c, js_max(16.0, f64::ceil(r * 2.0)) as usize);
            }
        }
    }

    fn emit_plot_candles(
        &self,
        b: &mut Batch,
        kind: CandleKind,
        bars: &[Ohlcv],
        style: Option<&CandleStyle>,
        bar_colors: Option<&[Option<CandleBarColor>]>,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        theme: &VelaTheme,
        off: f64,
    ) {
        let spacing = coords.body_spacing();
        let up = style
            .and_then(|s| s.up.as_deref())
            .unwrap_or_else(|| theme_str(theme, "upColor"));
        let down = style
            .and_then(|s| s.down.as_deref())
            .unwrap_or_else(|| theme_str(theme, "downColor"));
        let is_bar = kind == CandleKind::Bar;
        let half = js_max(0.5, f64::floor(spacing * 0.7) / 2.0);
        let tick_w = js_max(1.0, js_round(spacing * 0.35));
        let dpr = coords.dpr();
        let mut i = i0;
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let slot = cur - off;
            let Some(bar) = at(bars, slot) else { continue };
            if !(bar.open + bar.high + bar.low + bar.close).is_finite() {
                continue;
            }
            let bc: Option<&CandleBarColor> = bar_colors
                .and_then(|colors| at(colors, slot))
                .and_then(Option::as_ref);
            let dir = if bar.close >= bar.open { up } else { down };
            let body = parse_color(Some(bc.and_then(|c| c.color.as_deref()).unwrap_or(dir)));
            let x = js_round(coords.logical_to_x(cur));
            let h_y = y_of(coords, plane, bar.high);
            let l_y = y_of(coords, plane, bar.low);
            let o_y = y_of(coords, plane, bar.open);
            let c_y = y_of(coords, plane, bar.close);
            if is_bar {
                b.rect(x - 0.5, h_y, 1.0, l_y - h_y, body);
                b.rect(x - tick_w, o_y - 0.5, tick_w, 1.0, body);
                b.rect(x, c_y - 0.5, tick_w, 1.0, body);
                continue;
            }
            let w_dev = js_max(1.0, js_round(wick_width(spacing) * dpr));
            let wx = (js_round(x * dpr) - (to_int32(w_dev) >> 1) as f64) / dpr;
            let wick = bc
                .and_then(|c| c.wick_color.as_deref())
                .or_else(|| bc.and_then(|c| c.color.as_deref()))
                .unwrap_or(dir);
            b.rect(wx, h_y, w_dev / dpr, l_y - h_y, parse_color(Some(wick)));
            let top = js_min(o_y, c_y);
            let h = js_max(1.0, (c_y - o_y).abs());
            b.rect(x - half, top, half * 2.0, h, body);
            if let Some(border) = bc
                .and_then(|c| c.border_color.as_deref())
                .filter(|c| !c.is_empty())
            {
                let bord = parse_color(Some(border));
                b.rect(x - half, top, half * 2.0, 1.0, bord);
                b.rect(x - half, top + h - 1.0, half * 2.0, 1.0, bord);
                b.rect(x - half, top, 1.0, h, bord);
                b.rect(x + half - 1.0, top, 1.0, h, bord);
            }
        }
    }

    fn emit_hline(
        &self,
        b: &mut Batch,
        pl: &PriceLine,
        plane: &Plane,
        coords: &CoordinateSystem,
        data_w: f64,
        theme: &VelaTheme,
    ) {
        let y = js_round(y_of(coords, plane, pl.price));
        if y < plane.bounds.top || y > plane.bounds.top + plane.bounds.height {
            return;
        }
        b.dashed_seg(
            0.0,
            y,
            data_w,
            y,
            pl.width.unwrap_or(1.0),
            parse_color(Some(
                pl.color
                    .as_deref()
                    .unwrap_or_else(|| theme_str(theme, "textColor")),
            )),
            dash(pl.line_style.unwrap_or(LineStyle::Solid)),
        );
    }
}

impl RenderBackend for WebGL2Backend {
    fn kind(&self) -> BackendKind {
        BackendKind::WebGl2
    }

    fn model_alpha(&self) -> f64 {
        self.model_alpha
    }

    fn set_model_alpha(&mut self, alpha: f64) {
        self.model_alpha = alpha;
    }

    fn candle_body_alpha(&self) -> f64 {
        self.candle_body_alpha
    }

    fn set_candle_body_alpha(&mut self, alpha: f64) {
        self.candle_body_alpha = alpha;
    }

    fn candle_structure_alpha(&self) -> f64 {
        self.candle_structure_alpha
    }

    fn set_candle_structure_alpha(&mut self, alpha: f64) {
        self.candle_structure_alpha = alpha;
    }

    fn candle_body_scale(&self) -> f64 {
        self.candle_body_scale
    }

    fn set_candle_body_scale(&mut self, scale: f64) {
        self.candle_body_scale = scale;
    }

    fn render(&mut self, scene: &SceneGraph, coords: &CoordinateSystem, theme: &VelaTheme) {
        let Some(gl_rc) = self.gl.clone() else {
            return;
        };
        let gl: &dyn Gl = &*gl_rc;
        if self.program.is_none() || self.context_lost || gl.is_context_lost() {
            return;
        }
        let (canvas_w, canvas_h) = self.backing;
        let dpr = coords.dpr();
        let data_w = coords.width();

        gl.viewport(0, 0, canvas_w as i32, canvas_h as i32);
        gl.disable(consts::SCISSOR_TEST);
        // Transparent clear: the wrapper paints the chart background, so a reveal layer
        // behind this canvas shows through wherever no geometry is drawn.
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(consts::COLOR_BUFFER_BIT);

        let n = coords.bar_count();
        if n == 0 {
            return;
        }
        let vr = coords.visible_logical_range();
        let i0 = js_max(0.0, f64::floor(vr.from));
        let i1 = js_min((n - 1) as f64, f64::ceil(vr.to));
        // A NaN bound (a corrupt viewport) does not stop the frame, as in the original
        // (`i1 < i0` is false for NaN): every bar loop then runs zero times, while the
        // backgrounds, slices and price lines still draw.
        if i1 < i0 {
            return;
        }
        // Indicator series and fills may carry points past the last bar (a plot shifted
        // forward), so they paint through the visible right whitespace too.
        let s1 = js_max(i1, f64::ceil(vr.to));
        let price_range = (!i0.is_nan() && !i1.is_nan()).then_some((i0 as usize, i1 as usize));

        // Blend/scissor are enabled once; the program/VAO/uniform bindings live in each
        // pane's `flush`, since an interleave-layer quad swaps them mid-pane anyway.
        gl.enable(consts::BLEND);
        gl.enable(consts::SCISSOR_TEST);

        let bar_color_map = merge_bar_colors(scene.indicators.values());
        let panes = scene.ordered_panes();
        let mut live_slices: Vec<usize> = Vec::new();
        let mut b = std::mem::take(&mut self.batch);
        for pane in panes {
            b.reset();
            // Pane scissor up-front: geometry flushes AND interleave-layer quads clip to it.
            let top_dev = js_round(pane.bounds.top * dpr);
            let bot_dev = js_round((pane.bounds.top + pane.bounds.height) * dpr);
            gl.scissor(
                0,
                (canvas_h - bot_dev) as i32,
                js_round(data_w * dpr) as i32,
                (bot_dev - top_dev) as i32,
            );
            // User-drawing interleave layers, prepainted by the renderer: each composites
            // just before the series carrying its `before_z`: under the candles, between
            // indicators.
            let slices = if pane.collapsed {
                &[][..]
            } else {
                scene
                    .drawing_slices
                    .get(&pane.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            };
            let mut si = 0;
            // z-order within the batch (painter's): bg, then the candles + each
            // indicator's fills + series interleaved by `scene.candle_z`/per-indicator z,
            // then hlines. (The grid + session highlights live on the renderer's backdrop
            // canvas below.)
            b.alpha = 1.0;
            // A collapsed pane is a legend-only strip: no plots (legend + separator are
            // chrome).
            if !pane.collapsed {
                let models = scene.ordered_indicators_for_pane(&pane.id);
                let is_price = pane.kind == PaneKind::Price;
                let pane_plane = Plane::of(pane);
                // Merged (own-scale) indicators draw against their own price window inside
                // the pane.
                let eff_plane = |m: &IndicatorModel| Plane {
                    scale: scene.scale_for(m, pane),
                    bounds: pane.bounds,
                };
                b.alpha = self.model_alpha; // indicator models fade in after the intro; candles/grid stay opaque
                // Behind everything: bgcolor spans, since Pine keeps the background under
                // every layer regardless of stacking. Own (non-force_overlay) spans on each
                // model's pane; force_overlay spans on the price pane (Pine semantics,
                // mirrors the chrome's overlay-drawing routing). Fills are NOT here: they
                // paint inside the z loop at their model's slot.
                for m in &models {
                    for bg in &m.backgrounds {
                        if !bg.overlay {
                            self.emit_background(&mut b, bg, &pane_plane, coords);
                        }
                    }
                }
                if is_price {
                    for m in scene.indicators.values() {
                        for bg in &m.backgrounds {
                            if bg.overlay {
                                self.emit_background(&mut b, bg, &pane_plane, coords);
                            }
                        }
                    }
                }
                // When the price is hidden, the candle layer is skipped entirely (overlays
                // still draw).
                let draw_candles = is_price && !scene.candles_hidden;
                let mut candle_drawn = false;
                for m in &models {
                    if draw_candles && !candle_drawn && scene.z_of(&m.id) >= scene.candle_z {
                        self.draw_slices_up_to(
                            gl,
                            &mut b,
                            dpr,
                            slices,
                            &mut si,
                            scene.candle_z,
                            &mut live_slices,
                        );
                        b.alpha = self.candle_structure_alpha; // baseline; emit_candles sets body vs structure per-element
                        if let Some((first, last)) = price_range {
                            self.emit_price_series(
                                &mut b,
                                scene,
                                first,
                                last,
                                coords,
                                &pane_plane,
                                theme,
                                &bar_color_map,
                                data_w,
                            );
                        }
                        candle_drawn = true;
                    }
                    self.draw_slices_up_to(
                        gl,
                        &mut b,
                        dpr,
                        slices,
                        &mut si,
                        scene.z_of(&m.id),
                        &mut live_slices,
                    );
                    b.alpha = self.model_alpha;
                    // Model data is index-aligned from the model's ANCHOR bar (offset 0 =
                    // whole-chart).
                    let off = scene.offset_of(&m.id) as f64;
                    let mp = eff_plane(m);
                    // Fills paint at the model's z slot, under its own series: a band
                    // between two plots sits behind the plot lines, and the whole model
                    // moves as one unit when its object-tree row is reordered.
                    for f in &m.fills {
                        if !f.overlay {
                            self.emit_fill(&mut b, m, f, &mp, coords, i0, s1, off);
                        }
                    }
                    for s in &m.series {
                        if !s.overlay {
                            self.emit_series(&mut b, s, &mp, coords, i0, s1, theme, off);
                        }
                    }
                }
                if draw_candles && !candle_drawn {
                    self.draw_slices_up_to(
                        gl,
                        &mut b,
                        dpr,
                        slices,
                        &mut si,
                        scene.candle_z,
                        &mut live_slices,
                    );
                    b.alpha = self.candle_structure_alpha;
                    if let Some((first, last)) = price_range {
                        self.emit_price_series(
                            &mut b,
                            scene,
                            first,
                            last,
                            coords,
                            &pane_plane,
                            theme,
                            &bar_color_map,
                            data_w,
                        );
                    }
                }
                b.alpha = self.model_alpha;
                // force_overlay content from EVERY indicator paints on the price pane, at
                // the top of its series stack, against the master price scale: fills first
                // so a forced band still sits under the forced plot lines.
                if is_price {
                    for m in scene.indicators.values() {
                        let off = scene.offset_of(&m.id) as f64;
                        for f in &m.fills {
                            if f.overlay {
                                self.emit_fill(&mut b, m, f, &pane_plane, coords, i0, s1, off);
                            }
                        }
                    }
                    for m in scene.indicators.values() {
                        let off = scene.offset_of(&m.id) as f64;
                        for s in &m.series {
                            if s.overlay {
                                self.emit_series(
                                    &mut b,
                                    s,
                                    &pane_plane,
                                    coords,
                                    i0,
                                    s1,
                                    theme,
                                    off,
                                );
                            }
                        }
                    }
                }
                // Stack-top slices: the topmost indicator's drawings, force_overlay
                // drawings, and layers bound to a hidden/removed series: above the forced
                // series too, since drawings always paint over their own plots.
                self.draw_slices_up_to(
                    gl,
                    &mut b,
                    dpr,
                    slices,
                    &mut si,
                    f64::INFINITY,
                    &mut live_slices,
                );
                for m in &models {
                    let mp = eff_plane(m);
                    for pl in &m.price_lines {
                        self.emit_hline(&mut b, pl, &mp, coords, data_w, theme);
                    }
                }
                b.alpha = 1.0;
            }
            self.flush(gl, &mut b, dpr);
        }
        self.batch = b;
        gl.disable(consts::SCISSOR_TEST);
        // Drop textures whose layer disappeared this frame (the canvas cache in the
        // drawings controller prunes the same way, so this stays a handful of entries).
        let mut kept = Vec::with_capacity(self.slice_tex.len());
        for (key, tex) in std::mem::take(&mut self.slice_tex) {
            if live_slices.contains(&key) {
                kept.push((key, tex));
            } else {
                gl.delete_texture(tex);
            }
        }
        self.slice_tex = kept;

        // Build the glow program on first use: it is compiled in init_gl only when glow
        // starts > 0, so a runtime toggle (off to on) must compile it lazily here,
        // otherwise the bloom never appears until the next full backend (re)init.
        // Disable glow if it cannot compile.
        if self.glow > 0.0 && self.screen_program.is_none() && !self.ensure_screen_pipeline() {
            self.glow = 0.0;
        }
        if self.glow > 0.0 && self.screen_program.is_some() {
            self.glow_batch.alpha = self.model_alpha; // the glow of faded series fades too
            self.render_glow(gl, scene, coords, i0, s1);
        }
    }

    fn destroy(&mut self) {
        if let Some(gl_rc) = self.gl.take() {
            let gl: &dyn Gl = &*gl_rc;
            if let Some(p) = self.program {
                gl.delete_program(p);
            }
            if let Some(v) = self.vbo {
                gl.delete_buffer(v);
            }
            if let Some(v) = self.vao {
                gl.delete_vertex_array(v);
            }
            if let Some(p) = self.screen_program {
                gl.delete_program(p);
            }
            if let Some(v) = self.quad_vbo {
                gl.delete_buffer(v);
            }
            if let Some(v) = self.quad_vao {
                gl.delete_vertex_array(v);
            }
            for (_, tex) in std::mem::take(&mut self.slice_tex) {
                gl.delete_texture(tex);
            }
            self.delete_fbos(gl);
            // Release the CONTEXT itself, not just its resources: browsers cap live WebGL
            // contexts per page (~16) and reclaim destroyed ones LAZILY. Without an
            // explicit loseContext(), chart create/destroy cycles (SPA navigation,
            // workspace layout churn) evict the OLDEST context, which can be a
            // still-visible chart's.
            gl.lose_context();
        }
        self.program = None;
        self.vao = None;
        self.vbo = None;
        self.screen_program = None;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::model::{FillGradientStop, SeriesDisplay};
    use crate::render::backend::canvas2d_backend::Canvas2dBackend;
    use crate::render::backend::canvas2d_backend::test_support::{
        bars, coords_for, line_like, model, scene_with, theme,
    };
    use crate::render::canvas::RecordingCanvas;
    use crate::render::core::scene_graph::DrawingSlice;
    use crate::render::gl::RecordingGl;

    const UP: &str = "#00AA00";
    const DOWN: &str = "#AA0000";

    fn col(css: &str) -> Rgba {
        parse_color(Some(css))
    }

    /// The pixel y of a price on the 0..100 pane over 100 px, by the original's
    /// arithmetic (`top + height * (1 - t)`), whose rounding noise a literal would not
    /// carry.
    fn y(price: f64) -> f64 {
        0.0 + 100.0 * (1.0 - (price - 0.0) / (100.0 - 0.0))
    }

    /// Bar `i` of a 3-bar chart at spacing 20 on a 100 px plot: x = 60, 80, 100.
    fn x(i: usize) -> f64 {
        100.0 - (2.0 - i as f64) * 20.0
    }

    /// Up, down, flat (zero range).
    fn scene3() -> SceneGraph {
        scene_with(
            bars(&[
                (40.0, 70.0, 30.0, 60.0),
                (60.0, 80.0, 20.0, 30.0),
                (50.0, 50.0, 50.0, 50.0),
            ]),
            0.0,
            100.0,
            0.0,
            100.0,
        )
    }

    fn coords3(scene: &SceneGraph) -> CoordinateSystem {
        coords_for(scene, 100.0, 100.0, 1.0, 20.0, 0.0)
    }

    fn mounted() -> (Rc<RecordingGl>, WebGL2Backend) {
        let gl = Rc::new(RecordingGl::new());
        let mut backend = WebGL2Backend::new();
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(backend.ok());
        gl.clear();
        (gl, backend)
    }

    /// One frame of `scene`, as the recorded calls.
    fn run(scene: &SceneGraph, coords: &CoordinateSystem) -> Vec<Value> {
        let (gl, mut backend) = mounted();
        backend.render(scene, coords, &theme());
        gl.ops()
    }

    fn buffer(b: &Batch) -> Value {
        Value::Array(b.view().iter().map(|v| json!(f64::from(*v))).collect())
    }

    fn open_ops() -> Vec<Value> {
        vec![
            json!(["isContextLost"]),
            json!(["viewport", 0, 0, 100, 100]),
            json!(["disable", 3089]),
            json!(["clearColor", 0.0, 0.0, 0.0, 0.0]),
            json!(["clear", 16384]),
            json!(["enable", 3042]),
            json!(["enable", 3089]),
        ]
    }

    fn flush_ops(b: &Batch) -> Vec<Value> {
        vec![
            json!(["blendFuncSeparate", 770, 771, 1, 771]),
            json!(["useProgram", "program#1"]),
            json!(["bindVertexArray", "vertexArray#1"]),
            json!(["uniform2f", "uniform#1", 100.0, 100.0]),
            json!(["bindBuffer", 34962, "buffer#1"]),
            json!(["bufferData", 34962, buffer(b), 35048]),
            json!(["drawArrays", 4, 0, b.vertex_count()]),
        ]
    }

    /// The whole frame of a single full-size pane that flushes `b` once.
    fn frame(b: &Batch) -> Value {
        let mut ops = open_ops();
        ops.push(json!(["scissor", 0, 0, 100, 100]));
        if b.vertex_count() > 0 {
            ops.extend(flush_ops(b));
        }
        ops.push(json!(["disable", 3089]));
        Value::Array(ops)
    }

    fn painted(scene: &SceneGraph) -> Value {
        Value::Array(run(scene, &coords3(scene)))
    }

    fn add_model(scene: &mut SceneGraph, m: IndicatorModel) {
        let id = m.id.clone();
        scene.indicators.insert(id.clone(), m);
        scene.assign_indicator_z(&id);
    }

    fn vals(values: &[f64]) -> Vec<Option<f64>> {
        values.iter().map(|v| Some(*v)).collect()
    }

    // ── mount, failure, context ──

    fn mount_ops() -> Vec<Value> {
        vec![
            json!(["createShader", 35633]),
            json!(["shaderSource", "shader#1", VERT_SRC]),
            json!(["compileShader", "shader#1"]),
            json!(["getShaderParameter", "shader#1", 35713]),
            json!(["createShader", 35632]),
            json!(["shaderSource", "shader#2", FRAG_SRC]),
            json!(["compileShader", "shader#2"]),
            json!(["getShaderParameter", "shader#2", 35713]),
            json!(["createProgram"]),
            json!(["attachShader", "program#1", "shader#1"]),
            json!(["attachShader", "program#1", "shader#2"]),
            json!(["linkProgram", "program#1"]),
            json!(["getProgramParameter", "program#1", 35714]),
            json!(["deleteShader", "shader#1"]),
            json!(["deleteShader", "shader#2"]),
            json!(["getUniformLocation", "program#1", "uRes"]),
            json!(["createBuffer"]),
            json!(["createVertexArray"]),
            json!(["bindVertexArray", "vertexArray#1"]),
            json!(["bindBuffer", 34962, "buffer#1"]),
            json!(["enableVertexAttribArray", 0]),
            json!(["vertexAttribPointer", 0, 2, 5126, false, 32, 0]),
            json!(["enableVertexAttribArray", 1]),
            json!(["vertexAttribPointer", 1, 4, 5126, false, 32, 8]),
            json!(["enableVertexAttribArray", 2]),
            json!(["vertexAttribPointer", 2, 2, 5126, false, 32, 24]),
            json!(["bindVertexArray", Value::Null]),
        ]
    }

    /// The screen pipeline's build, with the object numbers it gets after the main
    /// program (shaders 3 and 4, program 2, uniforms 2 to 4, buffer 2, vertex array 2).
    fn screen_pipeline_ops() -> Vec<Value> {
        vec![
            json!(["createShader", 35633]),
            json!(["shaderSource", "shader#3", SCREEN_VERT]),
            json!(["compileShader", "shader#3"]),
            json!(["getShaderParameter", "shader#3", 35713]),
            json!(["createShader", 35632]),
            json!(["shaderSource", "shader#4", SCREEN_FRAG]),
            json!(["compileShader", "shader#4"]),
            json!(["getShaderParameter", "shader#4", 35713]),
            json!(["createProgram"]),
            json!(["attachShader", "program#2", "shader#3"]),
            json!(["attachShader", "program#2", "shader#4"]),
            json!(["linkProgram", "program#2"]),
            json!(["getProgramParameter", "program#2", 35714]),
            json!(["deleteShader", "shader#3"]),
            json!(["deleteShader", "shader#4"]),
            json!(["getUniformLocation", "program#2", "uTex"]),
            json!(["getUniformLocation", "program#2", "uDir"]),
            json!(["getUniformLocation", "program#2", "uIntensity"]),
            json!(["createBuffer"]),
            json!(["createVertexArray"]),
            json!(["bindVertexArray", "vertexArray#2"]),
            json!(["bindBuffer", 34962, "buffer#2"]),
            json!([
                "bufferData",
                34962,
                [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
                35044
            ]),
            json!(["enableVertexAttribArray", 0]),
            json!(["vertexAttribPointer", 0, 2, 5126, false, 0, 0]),
            json!(["bindVertexArray", Value::Null]),
        ]
    }

    #[test]
    fn the_shader_sources_are_the_originals() {
        assert!(VERT_SRC.starts_with("#version 300 es\nlayout(location=0) in vec2 aPos;"));
        assert!(VERT_SRC.contains(
            "vec2 clip = vec2(aPos.x / uRes.x * 2.0 - 1.0, 1.0 - aPos.y / uRes.y * 2.0);"
        ));
        assert!(FRAG_SRC.contains("float aa = fwidth(vEdge.x);"));
        assert!(FRAG_SRC.contains(
            "float cov = clamp((vEdge.y - abs(vEdge.x)) / max(aa, 1e-4) + 0.5, 0.0, 1.0);"
        ));
        assert!(SCREEN_VERT.contains("vUv = aPos * 0.5 + 0.5;"));
        assert!(SCREEN_FRAG.contains("vec2 o1 = uDir * 1.3846153846;"));
        assert!(SCREEN_FRAG.contains("sum += texture(uTex, vUv - o2) * 0.0702702703;"));
    }

    #[test]
    fn mounting_builds_the_program_and_the_vertex_layout() {
        let gl = Rc::new(RecordingGl::new());
        let mut backend = WebGL2Backend::new();
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(backend.ok());
        assert_eq!(gl.to_json(), Value::Array(mount_ops()));
        assert_eq!(backend.kind(), BackendKind::WebGl2);
    }

    #[test]
    fn mounting_with_glow_builds_the_screen_pipeline_too() {
        let gl = Rc::new(RecordingGl::new());
        let mut backend = WebGL2Backend::new();
        backend.glow = 1.0;
        backend.mount(gl.clone(), 100.0, 100.0);
        let mut expected = mount_ops();
        expected.extend(screen_pipeline_ops());
        assert_eq!(gl.to_json(), Value::Array(expected));
        assert_eq!(backend.glow, 1.0);
    }

    #[test]
    fn a_screen_pipeline_that_does_not_link_turns_glow_off_but_keeps_the_backend() {
        let gl = Rc::new(RecordingGl::new());
        gl.fail_nth_link(2);
        let mut backend = WebGL2Backend::new();
        backend.glow = 1.0;
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(backend.ok());
        assert_eq!(backend.glow, 0.0);
        let ops = gl.ops();
        assert_eq!(
            ops.last().unwrap(),
            &json!(["deleteProgram", "program#2"]),
            "a screen program that fails to link is deleted (its shaders are not, as in the original)"
        );
    }

    #[test]
    fn a_shader_that_does_not_compile_leaves_the_backend_unmounted() {
        let gl = Rc::new(RecordingGl::new());
        gl.set_compile_ok(false);
        let mut backend = WebGL2Backend::new();
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(!backend.ok());
        assert_eq!(
            gl.to_json(),
            json!([
                ["createShader", 35633],
                ["shaderSource", "shader#1", VERT_SRC],
                ["compileShader", "shader#1"],
                ["getShaderParameter", "shader#1", 35713],
                ["deleteShader", "shader#1"],
                ["createShader", 35632],
                ["shaderSource", "shader#2", FRAG_SRC],
                ["compileShader", "shader#2"],
                ["getShaderParameter", "shader#2", 35713],
                ["deleteShader", "shader#2"],
                ["createProgram"],
                ["deleteProgram", "program#1"],
            ])
        );
    }

    #[test]
    fn a_program_that_does_not_link_is_freed() {
        let gl = Rc::new(RecordingGl::new());
        gl.set_link_ok(false);
        let mut backend = WebGL2Backend::new();
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(!backend.ok());
        let ops = gl.ops();
        assert_eq!(
            &ops[ops.len() - 4..],
            &[
                json!(["getProgramParameter", "program#1", 35714]),
                json!(["deleteProgram", "program#1"]),
                json!(["deleteShader", "shader#1"]),
                json!(["deleteShader", "shader#2"]),
            ]
        );
    }

    #[test]
    fn an_object_the_context_cannot_create_fails_the_mount_without_leaks() {
        let gl = Rc::new(RecordingGl::new());
        gl.set_create_ok(false);
        let mut backend = WebGL2Backend::new();
        backend.mount(gl.clone(), 100.0, 100.0);
        assert!(!backend.ok());
        assert_eq!(
            gl.to_json(),
            json!([
                ["createShader", 35633],
                ["createShader", 35632],
                ["createProgram"]
            ])
        );
    }

    #[test]
    fn an_unmounted_backend_paints_nothing() {
        let mut backend = WebGL2Backend::new();
        let scene = scene3();
        backend.render(&scene, &coords3(&scene), &theme());
        assert!(!backend.ok());
    }

    #[test]
    fn a_chart_without_bars_only_clears() {
        let scene = scene_with(vec![], 0.0, 100.0, 0.0, 100.0);
        let mut coords = coords3(&scene);
        coords.set_bars(&[]);
        assert_eq!(
            Value::Array(run(&scene, &coords)),
            json!([
                ["isContextLost"],
                ["viewport", 0, 0, 100, 100],
                ["disable", 3089],
                ["clearColor", 0.0, 0.0, 0.0, 0.0],
                ["clear", 16384],
            ])
        );
    }

    #[test]
    fn a_lost_context_paints_nothing_until_restored() {
        let (gl, mut backend) = mounted();
        let scene = scene3();
        let coords = coords3(&scene);
        backend.on_context_lost();
        backend.render(&scene, &coords, &theme());
        assert!(gl.is_empty(), "no call, not even the isContextLost query");
        assert!(backend.on_context_restored());
        // The restore rebuilt the program and buffers (as new objects).
        let rebuilt: Vec<String> = gl.names();
        let first_build: Vec<String> = mount_ops()
            .iter()
            .map(|op| op[0].as_str().unwrap().to_string())
            .collect();
        assert_eq!(rebuilt, first_build);
        assert!(gl.calls("createProgram").len() == 1);
        assert_eq!(
            gl.calls("attachShader")[0],
            json!(["attachShader", "program#2", "shader#3"])
        );
        gl.clear();
        backend.render(&scene, &coords, &theme());
        assert_eq!(gl.names()[0], "isContextLost");
        assert!(gl.names().contains(&"drawArrays".to_string()));
        assert!(gl.calls("useProgram").iter().all(|c| c[1] == "program#2"));
    }

    #[test]
    fn a_context_the_driver_reports_lost_paints_nothing() {
        let (gl, mut backend) = mounted();
        gl.set_context_lost(true);
        let scene = scene3();
        backend.render(&scene, &coords3(&scene), &theme());
        assert_eq!(gl.to_json(), json!([["isContextLost"]]));
    }

    #[test]
    fn a_restore_that_cannot_rebuild_reports_failure() {
        let (gl, mut backend) = mounted();
        backend.on_context_lost();
        gl.set_link_ok(false);
        assert!(!backend.on_context_restored());
    }

    #[test]
    fn destroying_frees_everything_and_releases_the_context() {
        let (gl, mut backend) = mounted();
        backend.destroy();
        assert_eq!(
            gl.to_json(),
            json!([
                ["deleteProgram", "program#1"],
                ["deleteBuffer", "buffer#1"],
                ["deleteVertexArray", "vertexArray#1"],
                ["getExtension", "WEBGL_lose_context"],
                ["loseContext"],
            ])
        );
        assert!(!backend.ok());
        gl.clear();
        let scene = scene3();
        backend.render(&scene, &coords3(&scene), &theme());
        assert!(gl.is_empty());
    }

    #[test]
    fn destroying_after_glow_frees_the_glow_objects_in_order() {
        let (gl, mut backend) = mounted();
        backend.glow = 1.0;
        let scene = scene3();
        backend.render(&scene, &coords3(&scene), &theme());
        gl.clear();
        backend.destroy();
        assert_eq!(
            gl.to_json(),
            json!([
                ["deleteProgram", "program#1"],
                ["deleteBuffer", "buffer#1"],
                ["deleteVertexArray", "vertexArray#1"],
                ["deleteProgram", "program#2"],
                ["deleteBuffer", "buffer#2"],
                ["deleteVertexArray", "vertexArray#2"],
                ["deleteFramebuffer", "framebuffer#1"],
                ["deleteFramebuffer", "framebuffer#2"],
                ["deleteTexture", "texture#1"],
                ["deleteTexture", "texture#2"],
                ["getExtension", "WEBGL_lose_context"],
                ["loseContext"],
            ])
        );
    }

    #[test]
    fn the_alphas_and_scale_are_plain_state() {
        let mut b = WebGL2Backend::new();
        assert_eq!(
            (
                b.model_alpha(),
                b.candle_body_alpha(),
                b.candle_structure_alpha(),
                b.candle_body_scale(),
                b.glow
            ),
            (1.0, 1.0, 1.0, 1.0, 0.0)
        );
        b.set_model_alpha(0.5);
        b.set_candle_body_alpha(0.25);
        b.set_candle_structure_alpha(0.75);
        b.set_candle_body_scale(0.07);
        assert_eq!(
            (
                b.model_alpha(),
                b.candle_body_alpha(),
                b.candle_structure_alpha(),
                b.candle_body_scale()
            ),
            (0.5, 0.25, 0.75, 0.07)
        );
    }

    #[test]
    fn the_workspace_budget_decides_only_auto() {
        use NativeBackend::*;
        assert_eq!(backend_mode_for_cells(None, 4, None), Auto);
        assert_eq!(backend_mode_for_cells(None, 8, None), Auto);
        assert_eq!(backend_mode_for_cells(None, 9, None), Canvas2d);
        assert_eq!(backend_mode_for_cells(Some(Auto), 9, None), Canvas2d);
        assert_eq!(backend_mode_for_cells(Some(Webgl2), 99, Some(2)), Webgl2);
        assert_eq!(backend_mode_for_cells(Some(Canvas2d), 1, None), Canvas2d);
        assert_eq!(backend_mode_for_cells(None, 3, Some(2)), Canvas2d);
        assert_eq!(backend_mode_for_cells(None, 2, Some(2)), Auto);
        assert_eq!(backend_mode_for_cells(None, 1, Some(0)), Canvas2d);
    }

    #[test]
    fn a_nan_visible_range_draws_no_bars_but_still_the_price_lines() {
        let mut scene = scene3();
        let mut m = model("ind", "price");
        m.price_lines = vec![PriceLine {
            id: "l".into(),
            pane_id: "price".into(),
            price: 50.0,
            color: Some("#ff00ff".into()),
            line_style: None,
            width: Some(2.0),
            title: None,
            display_title: None,
        }];
        m.series = vec![line_like(
            "l",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#f00",
            1.0,
        )];
        add_model(&mut scene, m);
        let mut coords = coords3(&scene);
        coords.set_viewport(crate::render::core::ViewportState {
            bar_spacing: 0.0,
            right_offset: 0.0,
        });
        assert!(coords.visible_logical_range().to.is_nan());
        let mut e = Batch::new();
        e.dashed_seg(0.0, 50.0, 100.0, 50.0, 2.0, col("#ff00ff"), None);
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    // ── candles ──

    #[test]
    fn candles_draw_wick_halves_around_the_body() {
        let (u, d) = (col(UP), col(DOWN));
        let mut e = Batch::new();
        // bar 0 at x = 60: wick 2 px wide at 59, body 14 px wide at 53
        e.rect(59.0, 30.0, 2.0, 10.0, u);
        e.rect(59.0, 60.0, 2.0, 10.0, u);
        e.rect(53.0, 40.0, 14.0, 20.0, u);
        // bar 1 at x = 80 (down); bar 2 is flat and skipped
        e.rect(79.0, 20.0, 2.0, 20.0, d);
        e.rect(79.0, 70.0, 2.0, 10.0, d);
        e.rect(73.0, 40.0, 14.0, 30.0, d);
        assert_eq!(painted(&scene3()), frame(&e));
    }

    fn one_bar_scene(ohlc: (f64, f64, f64, f64)) -> SceneGraph {
        scene_with(bars(&[ohlc]), 0.0, 100.0, 0.0, 100.0)
    }

    fn one_bar_coords(scene: &SceneGraph) -> CoordinateSystem {
        coords_for(scene, 100.0, 100.0, 1.0, 20.0, 0.0)
    }

    #[test]
    fn a_configured_border_strokes_the_body_inside_its_footprint() {
        let mut scene = one_bar_scene((40.0, 70.0, 30.0, 60.0));
        scene.style.candle.border_visible = true;
        scene.style.candle.border_up_color = Some("#ffffff".into());
        let coords = one_bar_coords(&scene);
        let (u, w) = (col(UP), col("#ffffff"));
        let mut e = Batch::new();
        // the only bar sits at x = 100
        e.rect(99.0, 30.0, 2.0, 10.0, u);
        e.rect(99.0, 60.0, 2.0, 10.0, u);
        e.rect(93.0, 40.0, 14.0, 20.0, u);
        e.rect_stroke(93.5, 40.5, 13.0, 19.0, 1.0, w);
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    #[test]
    fn a_fading_body_gets_a_hollow_outline_and_each_part_its_own_alpha() {
        let scene = one_bar_scene((40.0, 70.0, 30.0, 60.0));
        let coords = one_bar_coords(&scene);
        let (gl, mut backend) = mounted();
        backend.set_candle_body_alpha(0.5);
        backend.set_candle_structure_alpha(0.8);
        backend.render(&scene, &coords, &theme());
        let u = col(UP);
        let mut e = Batch::new();
        e.alpha = 0.8;
        e.rect(99.0, 30.0, 2.0, 10.0, u);
        e.rect(99.0, 60.0, 2.0, 10.0, u);
        e.alpha = 0.5;
        e.rect(93.0, 40.0, 14.0, 20.0, u);
        e.alpha = 0.8;
        e.rect_stroke(93.5, 40.5, 13.0, 19.0, 1.0, u);
        assert_eq!(gl.to_json(), frame(&e));
    }

    #[test]
    fn hidden_body_and_wick_leave_only_what_is_visible() {
        let mut scene = one_bar_scene((40.0, 70.0, 30.0, 60.0));
        scene.style.candle.body_visible = false;
        scene.style.candle.border_visible = true;
        scene.style.candle.wick_visible = false;
        let coords = one_bar_coords(&scene);
        let mut e = Batch::new();
        e.rect_stroke(93.5, 40.5, 13.0, 19.0, 1.0, col(UP));
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    #[test]
    fn a_barcolor_tints_the_body_not_the_wick_and_the_border_follows_it() {
        let mut scene = one_bar_scene((40.0, 70.0, 30.0, 60.0));
        scene.style.candle.border_visible = true;
        let mut m = model("ind", "price");
        m.bar_colors = vec![crate::model::BarColor {
            time: 0,
            color: "#abcdef".into(),
        }];
        add_model(&mut scene, m);
        let coords = one_bar_coords(&scene);
        let (u, t) = (col(UP), col("#abcdef"));
        let mut e = Batch::new();
        e.rect(99.0, 30.0, 2.0, 10.0, u);
        e.rect(99.0, 60.0, 2.0, 10.0, u);
        e.rect(93.0, 40.0, 14.0, 20.0, t);
        e.rect_stroke(93.5, 40.5, 13.0, 19.0, 1.0, t);
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    #[test]
    fn a_doji_keeps_a_one_pixel_body() {
        let scene = one_bar_scene((50.0, 70.0, 30.0, 50.0));
        let coords = one_bar_coords(&scene);
        let u = col(UP);
        let mut e = Batch::new();
        e.rect(99.0, 30.0, 2.0, 20.0, u);
        e.rect(99.0, 51.0, 2.0, 19.0, u);
        e.rect(93.0, 50.0, 14.0, 1.0, u);
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    #[test]
    fn the_wick_tier_draws_one_stick_per_bar() {
        let scene = scene3();
        let coords = coords_for(&scene, 100.0, 100.0, 1.0, 2.0, 0.0);
        let (u, d) = (col(UP), col(DOWN));
        let mut e = Batch::new();
        e.rect(96.0, 30.0, 1.0, 40.0, u);
        e.rect(98.0, 20.0, 1.0, 60.0, d);
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&e));
    }

    #[test]
    fn the_aggregate_tier_matches_the_canvas2d_sticks() {
        let scene = scene_with(
            bars(&[
                (40.0, 70.0, 30.0, 60.0),
                (60.0, 80.0, 20.0, 30.0),
                (50.0, 65.0, 45.0, 60.0),
                (60.0, 90.0, 10.0, 70.0),
            ]),
            0.0,
            100.0,
            0.0,
            100.0,
        );
        let coords = coords_for(&scene, 100.0, 100.0, 1.0, 0.4, 0.0);
        let ops = run(&scene, &coords);
        let sticks = ops
            .iter()
            .find(|op| op[0] == "bufferData")
            .map(|op| op[2].as_array().unwrap().clone())
            .unwrap();
        // canvas2d: moveTo(x + 0.5, high), lineTo(x + 0.5, low) per stick.
        let canvas = Rc::new(RecordingCanvas::new());
        let mut c2 = Canvas2dBackend::new();
        c2.mount(canvas.clone(), 100.0, 100.0);
        c2.render(&scene, &coords, &theme());
        let moves = canvas.calls("moveTo");
        let lines = canvas.calls("lineTo");
        assert!(!moves.is_empty());
        assert_eq!(sticks.len(), moves.len() * 6 * 8);
        for (k, (mv, ln)) in moves.iter().zip(&lines).enumerate() {
            let base = k * 48;
            let gl_center = sticks[base].as_f64().unwrap() + 0.5; // rect x + 0.5
            let gl_top = sticks[base + 1].as_f64().unwrap();
            let gl_bottom = sticks[base + 17].as_f64().unwrap(); // vertex 2's y
            assert!((mv[1].as_f64().unwrap() - (gl_center + 0.5)).abs() < 1e-3);
            assert!((mv[2].as_f64().unwrap() - gl_top).abs() < 1e-3);
            assert!((ln[2].as_f64().unwrap() - gl_bottom).abs() < 1e-3);
        }
    }

    // ── chart types ──

    #[test]
    fn ohlc_bars_are_a_stick_and_two_ticks() {
        let mut scene = scene3();
        scene.price_style = "bars".into();
        let (u, d) = (col(UP), col(DOWN));
        let mut e = Batch::new();
        e.rect(59.5, y(70.0), 1.0, y(30.0) - y(70.0), u);
        e.rect(53.0, y(40.0) - 0.5, 7.0, 1.0, u);
        e.rect(60.0, y(60.0) - 0.5, 7.0, 1.0, u);
        e.rect(79.5, y(80.0), 1.0, y(20.0) - y(80.0), d);
        e.rect(73.0, y(60.0) - 0.5, 7.0, 1.0, d);
        e.rect(80.0, y(30.0) - 0.5, 7.0, 1.0, d);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn the_line_style_joins_each_vertex_with_a_round_circle() {
        let mut scene = scene3();
        scene.price_style = "line".into();
        let c = col(crate::palette::SERIES_LINE);
        let mut e = Batch::new();
        e.circle(60.0, y(60.0), 1.0, c, 6);
        e.seg(60.0, y(60.0), 80.0, y(30.0), 2.0, c);
        e.circle(80.0, y(30.0), 1.0, c, 6);
        e.seg(80.0, y(30.0), 100.0, y(50.0), 2.0, c);
        e.circle(100.0, y(50.0), 1.0, c, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn line_markers_add_a_dot_per_close_when_spaced() {
        let mut scene = scene3();
        scene.price_style = "linemarkers".into();
        let c = col(crate::palette::SERIES_LINE);
        let mut e = Batch::new();
        e.circle(60.0, y(60.0), 1.0, c, 6);
        e.seg(60.0, y(60.0), 80.0, y(30.0), 2.0, c);
        e.circle(80.0, y(30.0), 1.0, c, 6);
        e.seg(80.0, y(30.0), 100.0, y(50.0), 2.0, c);
        e.circle(100.0, y(50.0), 1.0, c, 6);
        for (i, close) in [60.0, 30.0, 50.0].into_iter().enumerate() {
            e.circle(x(i), y(close), 4.0, c, 16);
        }
        assert_eq!(painted(&scene), frame(&e));
        // Dense bars keep the plain line: 3 circles and 2 segments.
        let coords = coords_for(&scene, 100.0, 100.0, 1.0, 6.0, 0.0);
        let ops = run(&scene, &coords);
        let verts = ops.iter().find(|op| op[0] == "drawArrays").unwrap()[3]
            .as_u64()
            .unwrap();
        assert_eq!(verts, 3 * 18 + 2 * 6);
    }

    #[test]
    fn the_step_line_is_axis_aligned_rects_through_the_snapped_steps() {
        let mut scene = scene3();
        scene.price_style = "stepline".into();
        let c = col(crate::palette::SERIES_LINE);
        let mut e = Batch::new();
        e.rect(59.0, 39.0, 22.0, 2.0, c);
        e.rect(79.0, 41.0, 2.0, 30.0, c);
        e.rect(81.0, 69.0, 20.0, 2.0, c);
        e.rect(99.0, 49.0, 2.0, 20.0, c);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn columns_rise_from_the_pane_bottom_colored_against_the_previous_close() {
        let mut scene = scene3();
        scene.price_style = "columns".into();
        let up = col(&with_alpha(UP, COLUMNS_FILL_ALPHA));
        let down = col(&with_alpha(DOWN, COLUMNS_FILL_ALPHA));
        let mut e = Batch::new();
        e.rect(50.0, 40.0, 19.0, 60.0, up);
        e.rect(70.0, 70.0, 19.0, 30.0, down);
        e.rect(90.0, 50.0, 19.0, 50.0, up);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn the_area_style_fades_the_fill_to_the_floor_under_the_line() {
        let mut scene = scene3();
        scene.price_style = "area".into();
        let line = crate::palette::SERIES_LINE;
        let top = col(&with_alpha(line, 0.28));
        let bottom = col(&with_alpha(line, 0.02));
        let lc = col(line);
        let min_y = y(60.0);
        let span = 100.0 - min_y;
        let at = |py: f64| lerp_color(top, bottom, clamp01((py - min_y) / span));
        let mut e = Batch::new();
        e.quad4(
            60.0,
            y(60.0),
            at(y(60.0)),
            80.0,
            y(30.0),
            at(y(30.0)),
            80.0,
            100.0,
            bottom,
            60.0,
            100.0,
            bottom,
        );
        e.quad4(
            80.0,
            y(30.0),
            at(y(30.0)),
            100.0,
            y(50.0),
            at(y(50.0)),
            100.0,
            100.0,
            bottom,
            80.0,
            100.0,
            bottom,
        );
        e.circle(60.0, y(60.0), 1.0, lc, 6);
        e.seg(60.0, y(60.0), 80.0, y(30.0), 2.0, lc);
        e.circle(80.0, y(30.0), 1.0, lc, 6);
        e.seg(80.0, y(30.0), 100.0, y(50.0), 2.0, lc);
        e.circle(100.0, y(50.0), 1.0, lc, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn the_baseline_style_splits_fills_and_strokes_at_the_crossing() {
        let mut scene = scene3();
        scene.price_style = "baseline".into();
        let tl = BASELINE_TOP_LINE;
        let bl = BASELINE_BOTTOM_LINE;
        let (top_line, bottom_line) = (col(tl), col(bl));
        let top_fill = col(&with_alpha(tl, BASELINE_FILL_ALPHA));
        let top_fill2 = col(&with_alpha(tl, BASELINE_FILL_ALPHA_FAR));
        let bottom_fill = col(&with_alpha(bl, BASELINE_FILL_ALPHA));
        let bottom_fill2 = col(&with_alpha(bl, BASELINE_FILL_ALPHA_FAR));
        let base_y = y(50.0);
        let far = |py: f64| {
            if py <= base_y {
                top_fill2
            } else {
                bottom_fill2
            }
        };
        let side = |py: f64| {
            let above = py <= base_y;
            let region = if above { base_y - 0.0 } else { 100.0 - base_y };
            let t = clamp01((py - base_y).abs() / region);
            lerp_color(
                if above { top_fill2 } else { bottom_fill2 },
                if above { top_fill } else { bottom_fill },
                t,
            )
        };
        let pts = [(60.0, y(60.0)), (80.0, y(30.0)), (100.0, y(50.0))];
        let cross = |a: (f64, f64), c: (f64, f64)| {
            let t = (base_y - a.1) / (c.1 - a.1);
            a.0 + (c.0 - a.0) * t
        };
        let mut e = Batch::new();
        // both spans cross the baseline (the last close lands exactly on it: "above")
        for k in 1..3 {
            let (a, c) = (pts[k - 1], pts[k]);
            let xc = cross(a, c);
            e.tri(
                a.0,
                a.1,
                side(a.1),
                xc,
                base_y,
                far(a.1),
                a.0,
                base_y,
                far(a.1),
            );
            e.tri(
                xc,
                base_y,
                far(c.1),
                c.0,
                c.1,
                side(c.1),
                c.0,
                base_y,
                far(c.1),
            );
        }
        e.dashed_seg(
            0.0,
            base_y,
            100.0,
            base_y,
            1.0,
            col("#333333"),
            Some([6.0, 4.0]),
        );
        let (a, c) = (pts[0], pts[1]);
        let xc = cross(a, c);
        e.seg(a.0, a.1, xc, base_y, 2.0, top_line);
        e.seg(xc, base_y, c.0, c.1, 2.0, bottom_line);
        let (a, c) = (pts[1], pts[2]);
        let xc = cross(a, c);
        e.seg(a.0, a.1, xc, base_y, 2.0, bottom_line);
        e.seg(xc, base_y, c.0, c.1, 2.0, top_line);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_plugin_that_replaces_the_price_series_draws_none() {
        let mut scene = scene3();
        scene.base_painting = BasePainting::None;
        assert_eq!(painted(&scene), frame(&Batch::new()));
    }

    #[test]
    fn hidden_candles_leave_the_indicators() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        assert_eq!(painted(&scene), frame(&Batch::new()));
    }

    // ── indicator series ──

    fn with_series(series: Vec<SeriesSpec>) -> SceneGraph {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        m.series = series;
        add_model(&mut scene, m);
        scene
    }

    #[test]
    fn a_line_series_is_segments_with_round_caps_and_joins() {
        let scene = with_series(vec![line_like(
            "l",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0, 25.0]),
            "#ff0000",
            2.0,
        )]);
        let c = col("#ff0000");
        let mut e = Batch::new();
        e.seg(60.0, y(20.0), 80.0, y(30.0), 2.0, c);
        e.circle(60.0, y(20.0), 1.0, c, 6);
        e.circle(80.0, y(30.0), 1.0, c, 6);
        e.seg(80.0, y(30.0), 100.0, y(25.0), 2.0, c);
        e.circle(100.0, y(25.0), 1.0, c, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_gap_ends_a_run_and_a_point_color_overrides_the_series_color() {
        let mut spec = line_like(
            "l",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0), None],
            "#ff0000",
            1.0,
        );
        if let SeriesData::LineLike { points, .. } = &mut spec.data {
            points[1].color = Some("#00ff00".into());
        }
        let scene = with_series(vec![spec]);
        let (red, green) = (col("#ff0000"), col("#00ff00"));
        let mut e = Batch::new();
        // the segment entering point 1 takes point 1's color; the cap at its start takes
        // point 0's
        e.seg(60.0, y(20.0), 80.0, y(30.0), 1.0, green);
        e.circle(60.0, y(20.0), 0.5, red, 6);
        e.circle(80.0, y(30.0), 0.5, green, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn dashed_and_dotted_lines_keep_butt_ends() {
        for (style, pattern) in [
            (LineStyle::Dashed, [6.0, 4.0]),
            (LineStyle::Dotted, [2.0, 3.0]),
        ] {
            let mut spec = line_like(
                "l",
                LineLikeKind::Line,
                &vals(&[20.0, 30.0]),
                "#0000ff",
                2.0,
            );
            if let SeriesData::LineLike { style: s, .. } = &mut spec.data {
                s.line_style = style;
            }
            let scene = with_series(vec![spec]);
            let mut e = Batch::new();
            e.dashed_seg(
                60.0,
                y(20.0),
                80.0,
                y(30.0),
                2.0,
                col("#0000ff"),
                Some(pattern),
            );
            assert_eq!(painted(&scene), frame(&e));
        }
    }

    #[test]
    fn a_step_series_is_a_flat_run_then_a_vertical_run() {
        let scene = with_series(vec![line_like(
            "s",
            LineLikeKind::Step,
            &vals(&[20.0, 30.0, 25.0]),
            "#ff0000",
            2.0,
        )]);
        let c = col("#ff0000");
        let mut e = Batch::new();
        e.seg(60.0, y(20.0), 80.0, y(20.0), 2.0, c);
        e.seg(80.0, y(20.0), 80.0, y(30.0), 2.0, c);
        e.seg(80.0, y(30.0), 100.0, y(30.0), 2.0, c);
        e.seg(100.0, y(30.0), 100.0, y(25.0), 2.0, c);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn an_area_series_fades_to_transparent_black_then_draws_its_line() {
        let scene = with_series(vec![line_like(
            "a",
            LineLikeKind::Area,
            &vals(&[20.0, 30.0, 25.0]),
            "#ff0000",
            2.0,
        )]);
        let c = col("#ff0000");
        let ys = [y(20.0), y(30.0), y(25.0)];
        let min_y = y(30.0);
        let span = 100.0 - min_y;
        let at = |py: f64| fade_alpha(c, clamp01((py - min_y) / span));
        let mut e = Batch::new();
        let zero = [0.0; 4];
        for (a, b) in [(0usize, 1usize), (1, 2)] {
            e.quad4(
                x(a),
                ys[a],
                at(ys[a]),
                x(b),
                ys[b],
                at(ys[b]),
                x(b),
                100.0,
                zero,
                x(a),
                100.0,
                zero,
            );
        }
        e.seg(60.0, y(20.0), 80.0, y(30.0), 2.0, c);
        e.circle(60.0, y(20.0), 1.0, c, 6);
        e.circle(80.0, y(30.0), 1.0, c, 6);
        e.seg(80.0, y(30.0), 100.0, y(25.0), 2.0, c);
        e.circle(100.0, y(25.0), 1.0, c, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn histogram_and_columns_are_rects_from_the_base() {
        for (kind, w) in [
            (LineLikeKind::Histogram, 16.0),
            (LineLikeKind::Columns, 14.0),
        ] {
            let mut spec = line_like("h", kind, &vals(&[20.0, 70.0, 0.0]), "#ff0000", 2.0);
            if let SeriesData::LineLike { style, .. } = &mut spec.data {
                style.base = Some(40.0);
            }
            let scene = with_series(vec![spec]);
            let c = col("#ff0000");
            let base_y = y(40.0);
            let mut e = Batch::new();
            for (i, v) in [20.0, 70.0, 0.0].into_iter().enumerate() {
                let vy = y(v);
                e.rect(
                    x(i) - w / 2.0,
                    f64::min(base_y, vy),
                    w,
                    f64::max(1.0, (vy - base_y).abs()),
                    c,
                );
            }
            assert_eq!(painted(&scene), frame(&e));
        }
    }

    #[test]
    fn circles_and_crosses_mark_each_point() {
        let c = col("#ff0000");
        let circles = with_series(vec![line_like(
            "c",
            LineLikeKind::Circles,
            &vals(&[20.0, 30.0, 25.0]),
            "#ff0000",
            3.0,
        )]);
        let mut e = Batch::new();
        for (i, v) in [20.0, 30.0, 25.0].into_iter().enumerate() {
            e.circle(x(i), y(v), 3.0, c, 16);
        }
        assert_eq!(painted(&circles), frame(&e));

        let crosses = with_series(vec![line_like(
            "c",
            LineLikeKind::Cross,
            &vals(&[20.0, 30.0, 25.0]),
            "#ff0000",
            0.5,
        )]);
        let mut e = Batch::new();
        for (i, v) in [20.0, 30.0, 25.0].into_iter().enumerate() {
            e.seg(x(i) - 1.5, y(v), x(i) + 1.5, y(v), 1.0, c);
            e.seg(x(i), y(v) - 1.5, x(i), y(v) + 1.5, 1.0, c);
        }
        assert_eq!(painted(&crosses), frame(&e));
    }

    fn candle_series(kind: CandleKind) -> SeriesSpec {
        let b = |o, h, l, c| Ohlcv::new(0, o, h, l, c, None);
        SeriesSpec {
            id: "pc".into(),
            title: "pc".into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Candle {
                kind,
                bars: vec![
                    b(40.0, 70.0, 30.0, 60.0),
                    b(60.0, 80.0, 20.0, 30.0),
                    b(f64::NAN, f64::NAN, f64::NAN, f64::NAN),
                ],
                style: None,
                bar_colors: Some(vec![
                    Some(CandleBarColor {
                        color: Some("#112233".into()),
                        wick_color: Some("#445566".into()),
                        border_color: Some("#778899".into()),
                    }),
                    None,
                    None,
                ]),
            },
        }
    }

    #[test]
    fn a_candle_series_has_a_snapped_wick_a_body_and_an_optional_border() {
        let scene = with_series(vec![candle_series(CandleKind::Candle)]);
        let (body, wick, border, d) = (col("#112233"), col("#445566"), col("#778899"), col(DOWN));
        let mut e = Batch::new();
        // bar 0: x = 60, the wick 2 device px wide, centered
        e.rect(59.0, y(70.0), 2.0, y(30.0) - y(70.0), wick);
        let top = f64::min(y(40.0), y(60.0));
        let h = f64::max(1.0, (y(60.0) - y(40.0)).abs());
        e.rect(53.0, top, 14.0, h, body);
        e.rect(53.0, top, 14.0, 1.0, border);
        e.rect(53.0, top + h - 1.0, 14.0, 1.0, border);
        e.rect(53.0, top, 1.0, h, border);
        e.rect(66.0, top, 1.0, h, border);
        // bar 1: no per-bar color: the direction color for wick and body
        e.rect(79.0, y(80.0), 2.0, y(20.0) - y(80.0), d);
        let top = f64::min(y(60.0), y(30.0));
        let h = f64::max(1.0, (y(30.0) - y(60.0)).abs());
        e.rect(73.0, top, 14.0, h, d);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_bar_series_is_a_stick_and_two_ticks() {
        let scene = with_series(vec![candle_series(CandleKind::Bar)]);
        let (body, d) = (col("#112233"), col(DOWN));
        let mut e = Batch::new();
        e.rect(59.5, y(70.0), 1.0, y(30.0) - y(70.0), body);
        e.rect(53.0, y(40.0) - 0.5, 7.0, 1.0, body);
        e.rect(60.0, y(60.0) - 0.5, 7.0, 1.0, body);
        e.rect(79.5, y(80.0), 1.0, y(20.0) - y(80.0), d);
        e.rect(73.0, y(60.0) - 0.5, 7.0, 1.0, d);
        e.rect(80.0, y(30.0) - 0.5, 7.0, 1.0, d);
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn off_pane_and_marker_series_are_not_drawn() {
        let mut hidden = line_like("h", LineLikeKind::Line, &vals(&[20.0, 30.0]), "#f00", 1.0);
        hidden.visible = Some(false);
        let mut off_pane = line_like("o", LineLikeKind::Line, &vals(&[20.0, 30.0]), "#f00", 1.0);
        off_pane.display = Some(SeriesDisplay {
            pane: Some(false),
            price_scale: None,
            legend: None,
            data_window: None,
        });
        let markers = SeriesSpec {
            data: SeriesData::Markers { markers: vec![] },
            ..hidden.clone()
        };
        let scene = with_series(vec![hidden, off_pane, markers]);
        assert_eq!(painted(&scene), frame(&Batch::new()));
    }

    #[test]
    fn a_series_is_index_aligned_from_its_anchor_bar() {
        let mut scene = with_series(vec![line_like(
            "l",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#ff0000",
            1.0,
        )]);
        scene.set_anchor_offset("ind", 1);
        let c = col("#ff0000");
        let mut e = Batch::new();
        // points 0 and 1 sit on bars 1 and 2
        e.seg(80.0, y(20.0), 100.0, y(30.0), 1.0, c);
        e.circle(80.0, y(20.0), 0.5, c, 6);
        e.circle(100.0, y(30.0), 0.5, c, 6);
        assert_eq!(painted(&scene), frame(&e));
    }

    // ── fills, backgrounds, price lines ──

    fn fill_scene(fill: Fill) -> SceneGraph {
        let mut hidden_a = line_like(
            "a",
            LineLikeKind::Line,
            &vals(&[60.0, 70.0, 65.0]),
            "#f00",
            1.0,
        );
        hidden_a.visible = Some(false);
        let mut hidden_b = line_like(
            "b",
            LineLikeKind::Line,
            &vals(&[40.0, 50.0, 45.0]),
            "#f00",
            1.0,
        );
        hidden_b.visible = Some(false);
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        m.series = vec![hidden_a, hidden_b];
        m.fills = vec![fill];
        add_model(&mut scene, m);
        scene
    }

    fn fill(color: Option<&str>) -> Fill {
        Fill {
            id: "f".into(),
            pane_id: "price".into(),
            from_series_id: "a".into(),
            to_series_id: "b".into(),
            color: color.map(str::to_string),
            colors: None,
            gradient: None,
            overlay: false,
        }
    }

    #[test]
    fn a_flat_fill_is_a_quad_per_bar_span() {
        let scene = fill_scene(fill(Some("#112233")));
        let c = col("#112233");
        let mut e = Batch::new();
        e.quad(
            60.0,
            y(60.0),
            80.0,
            y(70.0),
            80.0,
            y(50.0),
            60.0,
            y(40.0),
            c,
        );
        e.quad(
            80.0,
            y(70.0),
            100.0,
            y(65.0),
            100.0,
            y(45.0),
            80.0,
            y(50.0),
            c,
        );
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_fill_with_no_color_draws_nothing_and_per_bar_colors_style_by_the_left_column() {
        assert_eq!(painted(&fill_scene(fill(None))), frame(&Batch::new()));
        assert_eq!(painted(&fill_scene(fill(Some("")))), frame(&Batch::new()));

        let mut f = fill(None);
        f.colors = Some(vec![Some("#aa0000".into()), None]);
        let scene = fill_scene(f);
        let mut e = Batch::new();
        e.quad(
            60.0,
            y(60.0),
            80.0,
            y(70.0),
            80.0,
            y(50.0),
            60.0,
            y(40.0),
            col("#aa0000"),
        );
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_gradient_fill_interpolates_its_corner_colors() {
        let mut f = fill(None);
        f.gradient = Some(vec![
            Some(FillGradientStop {
                top_value: 80.0,
                bottom_value: 20.0,
                top_color: "#ff0000".into(),
                bottom_color: "#0000ff".into(),
            }),
            None,
        ]);
        let scene = fill_scene(f);
        let (tc, bc) = (col("#ff0000"), col("#0000ff"));
        let (y_top, y_bot) = (y(80.0), y(20.0));
        let span = y_bot - y_top;
        let at = |py: f64| lerp_color(tc, bc, clamp01((py - y_top) / span));
        let mut e = Batch::new();
        e.quad4(
            60.0,
            y(60.0),
            at(y(60.0)),
            80.0,
            y(70.0),
            at(y(70.0)),
            80.0,
            y(50.0),
            at(y(50.0)),
            60.0,
            y(40.0),
            at(y(40.0)),
        );
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_degenerate_gradient_is_nudged_one_pixel() {
        let mut f = fill(None);
        f.gradient = Some(vec![Some(FillGradientStop {
            top_value: 50.0,
            bottom_value: 50.0,
            top_color: "#ff0000".into(),
            bottom_color: "#0000ff".into(),
        })]);
        let scene = fill_scene(f);
        let (tc, bc) = (col("#ff0000"), col("#0000ff"));
        let y_top = y(50.0);
        let at = |py: f64| lerp_color(tc, bc, clamp01((py - y_top) / 1.0));
        let mut e = Batch::new();
        e.quad4(
            60.0,
            y(60.0),
            at(y(60.0)),
            80.0,
            y(70.0),
            at(y(70.0)),
            80.0,
            y(50.0),
            at(y(50.0)),
            60.0,
            y(40.0),
            at(y(40.0)),
        );
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn a_fill_with_a_missing_series_is_skipped() {
        let mut f = fill(Some("#112233"));
        f.to_series_id = "nope".into();
        assert_eq!(painted(&fill_scene(f)), frame(&Batch::new()));
    }

    #[test]
    fn a_background_span_tints_the_pane_across_its_time_range() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        let bg = |id: &str, from: i64, to: i64, color: &str| Background {
            id: id.into(),
            pane_id: "price".into(),
            from,
            to,
            color: color.into(),
            overlay: false,
        };
        m.backgrounds = vec![
            bg("b", 60_000, 120_000, "#112233"),
            // entirely left of the plot
            bg("gone", -600_000, -300_000, "#ffffff"),
            // empty span
            bg("empty", 60_000, 60_000, "#ffffff"),
        ];
        add_model(&mut scene, m);
        let mut e = Batch::new();
        e.rect(80.0, 0.0, 20.0, 100.0, col("#112233"));
        assert_eq!(painted(&scene), frame(&e));
    }

    #[test]
    fn price_lines_snap_to_a_whole_pixel_and_honor_style_width_and_color() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        let line = |price: f64,
                    color: Option<&str>,
                    style: Option<LineStyle>,
                    width: Option<f64>| PriceLine {
            id: "l".into(),
            pane_id: "price".into(),
            price,
            color: color.map(str::to_string),
            line_style: style,
            width,
            title: None,
            display_title: None,
        };
        m.price_lines = vec![
            line(50.0, Some("#ff00ff"), Some(LineStyle::Dashed), Some(2.0)),
            line(33.4, None, None, None),
            line(150.0, None, None, None), // above the pane: skipped
        ];
        add_model(&mut scene, m);
        let mut e = Batch::new();
        e.dashed_seg(
            0.0,
            50.0,
            100.0,
            50.0,
            2.0,
            col("#ff00ff"),
            Some([6.0, 4.0]),
        );
        e.dashed_seg(0.0, 67.0, 100.0, 67.0, 1.0, col("#cccccc"), None);
        assert_eq!(painted(&scene), frame(&e));
    }

    // ── order, alpha, panes ──

    fn candle_batch() -> Batch {
        let (u, d) = (col(UP), col(DOWN));
        let mut e = Batch::new();
        e.rect(59.0, 30.0, 2.0, 10.0, u);
        e.rect(59.0, 60.0, 2.0, 10.0, u);
        e.rect(53.0, 40.0, 14.0, 20.0, u);
        e.rect(79.0, 20.0, 2.0, 20.0, d);
        e.rect(79.0, 70.0, 2.0, 10.0, d);
        e.rect(73.0, 40.0, 14.0, 30.0, d);
        e
    }

    fn short_line(e: &mut Batch, color: &str) {
        let c = col(color);
        e.seg(60.0, y(20.0), 80.0, y(30.0), 1.0, c);
        e.circle(60.0, y(20.0), 0.5, c, 6);
        e.circle(80.0, y(30.0), 0.5, c, 6);
    }

    #[test]
    fn indicators_paint_behind_or_in_front_of_the_candles_by_z() {
        let mut scene = scene3();
        let mut back = model("back", "price");
        back.series = vec![line_like(
            "b",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#ff0000",
            1.0,
        )];
        let mut front = model("front", "price");
        front.series = vec![line_like(
            "f",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#00ff00",
            1.0,
        )];
        add_model(&mut scene, back);
        add_model(&mut scene, front);
        scene.set_indicator_z("front", 5.0);
        let mut e = Batch::new();
        short_line(&mut e, "#ff0000");
        let candles = candle_batch();
        let mut all: Vec<f32> = e.view().to_vec();
        all.extend_from_slice(candles.view());
        let mut tail = Batch::new();
        short_line(&mut tail, "#00ff00");
        all.extend_from_slice(tail.view());
        let ops = run(&scene, &coords3(&scene));
        let upload = ops.iter().find(|op| op[0] == "bufferData").unwrap();
        let expected: Vec<Value> = all.iter().map(|v| json!(f64::from(*v))).collect();
        assert_eq!(upload[2], Value::Array(expected));
        assert_eq!(ops.iter().filter(|op| op[0] == "drawArrays").count(), 1);
    }

    #[test]
    fn the_model_alpha_fades_indicators_and_leaves_candles_opaque() {
        let mut scene = scene3();
        let mut m = model("ind", "price");
        m.series = vec![line_like(
            "b",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#ff0000",
            1.0,
        )];
        add_model(&mut scene, m);
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        backend.set_model_alpha(0.5);
        backend.render(&scene, &coords, &theme());
        let mut e = Batch::new();
        e.alpha = 0.5;
        short_line(&mut e, "#ff0000");
        let mut all: Vec<f32> = e.view().to_vec();
        all.extend_from_slice(candle_batch().view());
        let mut full = Batch::new();
        full.alpha = 1.0;
        let expected: Vec<Value> = all.iter().map(|v| json!(f64::from(*v))).collect();
        let ops = gl.ops();
        let upload = ops.iter().find(|op| op[0] == "bufferData").unwrap();
        assert_eq!(upload[2], Value::Array(expected));
    }

    #[test]
    fn force_overlay_series_paint_on_the_price_pane_after_the_candles() {
        let mut scene = scene3();
        let mut m = model("ind", "study");
        let mut s = line_like(
            "f",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#00ff00",
            1.0,
        );
        s.overlay = true;
        m.series = vec![s];
        m.overlay = false;
        add_model(&mut scene, m);
        let ops = run(&scene, &coords3(&scene));
        let upload = ops.iter().find(|op| op[0] == "bufferData").unwrap();
        let mut e = Batch::new();
        e.alpha = 1.0;
        let mut all: Vec<f32> = candle_batch().view().to_vec();
        short_line(&mut e, "#00ff00");
        all.extend_from_slice(e.view());
        let expected: Vec<Value> = all.iter().map(|v| json!(f64::from(*v))).collect();
        assert_eq!(upload[2], Value::Array(expected));
    }

    #[test]
    fn each_pane_draws_under_its_own_scissor() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let study = scene.ensure_pane("study", PaneKind::Study, 1, Some(1.0));
        study.bounds.top = 120.0;
        study.bounds.height = 50.0;
        study.scale.min = 0.0;
        study.scale.max = 100.0;
        let mut m = model("ind", "study");
        m.overlay = false;
        let mut spec = line_like("s", LineLikeKind::Histogram, &vals(&[50.0]), "#ff0000", 1.0);
        spec.pane_id = "study".into();
        m.series = vec![spec];
        add_model(&mut scene, m);
        let coords = coords_for(&scene, 100.0, 170.0, 2.0, 20.0, 0.0);
        let (gl, mut backend) = mounted();
        backend.set_backing_size(200.0, 340.0);
        backend.render(&scene, &coords, &theme());
        // price: top 0 to 200 device px; study: 240 to 340
        assert_eq!(
            gl.calls("scissor"),
            vec![
                json!(["scissor", 0, 140, 200, 200]),
                json!(["scissor", 0, 0, 200, 100])
            ]
        );
        // one histogram bar on the study pane, the only geometry of the frame
        assert_eq!(gl.calls("drawArrays"), vec![json!(["drawArrays", 4, 0, 6])]);
        assert_eq!(
            gl.calls("uniform2f"),
            vec![json!(["uniform2f", "uniform#1", 100.0, 170.0])]
        );
        let rect_y = gl.calls("bufferData")[0][2][1].as_f64().unwrap();
        // value 50 on 0..100 over 120..170 is y 145; the bar runs down to the base y(0) = 170
        assert_eq!(rect_y, 145.0);
    }

    #[test]
    fn a_collapsed_pane_draws_nothing() {
        let mut scene = scene3();
        scene.panes.get_mut("price").unwrap().collapsed = true;
        assert_eq!(painted(&scene), frame(&Batch::new()));
    }

    // ── slices ──

    fn texture_ops_first_time() -> Vec<Value> {
        vec![
            json!(["createTexture"]),
            json!(["bindTexture", 3553, "texture#1"]),
            json!(["texParameteri", 3553, 10241, 9729]),
            json!(["texParameteri", 3553, 10240, 9729]),
            json!(["texParameteri", 3553, 10242, 33071]),
            json!(["texParameteri", 3553, 10243, 33071]),
        ]
    }

    fn texture_ops_upload() -> Vec<Value> {
        vec![
            json!(["activeTexture", 33984]),
            json!(["bindTexture", 3553, "texture#1"]),
            json!(["pixelStorei", 37440, true]),
            json!(["pixelStorei", 37441, true]),
            json!(["texImage2D", 3553, 0, 6408, 6408, 5121, "canvas"]),
            json!(["pixelStorei", 37440, false]),
            json!(["pixelStorei", 37441, false]),
            json!(["useProgram", "program#2"]),
            json!(["bindVertexArray", "vertexArray#2"]),
            json!(["uniform1i", "uniform#2", 0]),
            json!(["uniform2f", "uniform#3", 0.0, 0.0]),
            json!(["uniform1f", "uniform#4", 1.0]),
            json!(["blendFuncSeparate", 1, 771, 1, 771]),
            json!(["drawArrays", 5, 0, 4]),
            json!(["bindVertexArray", Value::Null]),
        ]
    }

    #[test]
    fn a_drawing_slice_composites_between_the_series_and_the_candles() {
        let mut scene = scene3();
        let mut m = model("ind", "price");
        m.series = vec![line_like(
            "b",
            LineLikeKind::Line,
            &vals(&[20.0, 30.0]),
            "#ff0000",
            1.0,
        )];
        add_model(&mut scene, m);
        let canvas: CanvasImage = Rc::new(());
        scene.drawing_slices.insert(
            "price".into(),
            vec![DrawingSlice {
                before_z: -0.5,
                canvas: canvas.clone(),
            }],
        );
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        backend.render(&scene, &coords, &theme());

        let mut series = Batch::new();
        short_line(&mut series, "#ff0000");
        let candles = candle_batch();

        let mut expected = open_ops();
        expected.push(json!(["scissor", 0, 0, 100, 100]));
        expected.extend(flush_ops(&series));
        expected.extend(screen_pipeline_ops());
        expected.extend(texture_ops_first_time());
        expected.extend(texture_ops_upload());
        expected.extend(flush_ops(&candles));
        expected.push(json!(["disable", 3089]));
        assert_eq!(gl.to_json(), Value::Array(expected));

        // The next frame reuses the texture and the pipeline and re-uploads the pixels.
        gl.clear();
        backend.render(&scene, &coords, &theme());
        let mut expected = open_ops();
        expected.push(json!(["scissor", 0, 0, 100, 100]));
        expected.extend(flush_ops(&series));
        expected.extend(texture_ops_upload());
        expected.extend(flush_ops(&candles));
        expected.push(json!(["disable", 3089]));
        assert_eq!(gl.to_json(), Value::Array(expected));

        // A frame without the slice frees its texture, after the panes.
        gl.clear();
        scene.drawing_slices.clear();
        backend.render(&scene, &coords, &theme());
        let ops = gl.ops();
        assert_eq!(
            &ops[ops.len() - 2..],
            &[
                json!(["disable", 3089]),
                json!(["deleteTexture", "texture#1"])
            ]
        );
    }

    #[test]
    fn a_stack_top_slice_needs_no_flush_when_nothing_accumulated() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        scene.drawing_slices.insert(
            "price".into(),
            vec![DrawingSlice {
                before_z: f64::INFINITY,
                canvas: Rc::new(()) as CanvasImage,
            }],
        );
        let coords = coords3(&scene);
        let ops = run(&scene, &coords);
        let mut expected = open_ops();
        expected.push(json!(["scissor", 0, 0, 100, 100]));
        expected.extend(screen_pipeline_ops());
        expected.extend(texture_ops_first_time());
        expected.extend(texture_ops_upload());
        expected.push(json!(["disable", 3089]));
        assert_eq!(Value::Array(ops), Value::Array(expected));

        // A collapsed pane skips its slices and plots.
        scene.panes.get_mut("price").unwrap().collapsed = true;
        assert_eq!(Value::Array(run(&scene, &coords)), frame(&Batch::new()));
    }

    #[test]
    fn a_slice_whose_screen_pipeline_cannot_build_is_skipped() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        scene.drawing_slices.insert(
            "price".into(),
            vec![DrawingSlice {
                before_z: f64::INFINITY,
                canvas: Rc::new(()) as CanvasImage,
            }],
        );
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        gl.fail_nth_link(2);
        backend.render(&scene, &coords, &theme());
        assert!(gl.calls("texImage2D").is_empty());
        assert_eq!(
            gl.calls("deleteProgram"),
            vec![json!(["deleteProgram", "program#2"])]
        );
    }

    // ── glow ──

    #[test]
    fn glow_blurs_the_line_series_at_half_resolution_and_adds_the_halo() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        m.series = vec![
            line_like(
                "l",
                LineLikeKind::Line,
                &vals(&[20.0, 30.0]),
                "#ff0000",
                1.0,
            ),
            line_like(
                "h",
                LineLikeKind::Histogram,
                &vals(&[20.0, 30.0]),
                "#00ff00",
                1.0,
            ),
        ];
        add_model(&mut scene, m);
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        backend.glow = 1.5;
        backend.render(&scene, &coords, &theme());

        let mut main = Batch::new();
        short_line(&mut main, "#ff0000");
        let green = col("#00ff00");
        for (i, v) in [20.0, 30.0].into_iter().enumerate() {
            main.rect(
                x(i) - 8.0,
                f64::min(100.0, y(v)),
                16.0,
                f64::max(1.0, (y(v) - 100.0).abs()),
                green,
            );
        }
        // the glow source keeps only the line (no histogram), at half resolution
        let mut glow_src = Batch::new();
        short_line(&mut glow_src, "#ff0000");

        let mut expected = open_ops();
        expected.push(json!(["scissor", 0, 0, 100, 100]));
        expected.extend(flush_ops(&main));
        expected.push(json!(["disable", 3089]));
        expected.extend(screen_pipeline_ops());
        // ensureFbos at 50 x 50
        for n in [1, 2] {
            expected.push(json!(["createTexture"]));
            expected.push(json!(["bindTexture", 3553, format!("texture#{n}")]));
            expected.push(json!([
                "texImage2D",
                3553,
                0,
                6408,
                50,
                50,
                0,
                6408,
                5121,
                Value::Null
            ]));
            expected.push(json!(["texParameteri", 3553, 10241, 9729]));
            expected.push(json!(["texParameteri", 3553, 10240, 9729]));
            expected.push(json!(["texParameteri", 3553, 10242, 33071]));
            expected.push(json!(["texParameteri", 3553, 10243, 33071]));
            expected.push(json!(["createFramebuffer"]));
            expected.push(json!([
                "bindFramebuffer",
                36160,
                format!("framebuffer#{n}")
            ]));
            expected.push(json!([
                "framebufferTexture2D",
                36160,
                36064,
                3553,
                format!("texture#{n}"),
                0
            ]));
        }
        expected.extend([
            json!(["bindFramebuffer", 36160, Value::Null]),
            json!(["bindFramebuffer", 36160, "framebuffer#1"]),
            json!(["viewport", 0, 0, 50, 50]),
            json!(["disable", 3089]),
            json!(["clearColor", 0.0, 0.0, 0.0, 0.0]),
            json!(["clear", 16384]),
            json!(["enable", 3042]),
            json!(["blendFuncSeparate", 770, 771, 1, 771]),
            json!(["useProgram", "program#1"]),
            json!(["bindVertexArray", "vertexArray#1"]),
            json!(["uniform2f", "uniform#1", 100.0, 100.0]),
            json!(["enable", 3089]),
            json!(["scissor", 0, 0, 50, 50]),
            json!(["bindBuffer", 34962, "buffer#1"]),
            json!(["bufferData", 34962, buffer(&glow_src), 35048]),
            json!(["drawArrays", 4, 0, glow_src.vertex_count()]),
            json!(["disable", 3089]),
            json!(["useProgram", "program#2"]),
            json!(["bindVertexArray", "vertexArray#2"]),
            json!(["uniform1i", "uniform#2", 0]),
            json!(["uniform1f", "uniform#4", 1.0]),
            json!(["activeTexture", 33984]),
            json!(["disable", 3042]),
            json!(["bindFramebuffer", 36160, "framebuffer#2"]),
            json!(["bindTexture", 3553, "texture#1"]),
            json!(["uniform2f", "uniform#3", 0.02, 0.0]),
            json!(["drawArrays", 5, 0, 4]),
            json!(["bindFramebuffer", 36160, "framebuffer#1"]),
            json!(["bindTexture", 3553, "texture#2"]),
            json!(["uniform2f", "uniform#3", 0.0, 0.02]),
            json!(["drawArrays", 5, 0, 4]),
            json!(["bindFramebuffer", 36160, Value::Null]),
            json!(["viewport", 0, 0, 100, 100]),
            json!(["enable", 3042]),
            json!(["blendFunc", 1, 1]),
            json!(["bindTexture", 3553, "texture#1"]),
            json!(["uniform2f", "uniform#3", 0.0, 0.0]),
            json!(["uniform1f", "uniform#4", 1.5]),
            json!(["drawArrays", 5, 0, 4]),
            json!(["bindTexture", 3553, Value::Null]),
            json!(["bindVertexArray", Value::Null]),
        ]);
        assert_eq!(gl.to_json(), Value::Array(expected));

        // The next frame reuses the pipeline and the targets.
        gl.clear();
        backend.render(&scene, &coords, &theme());
        let names = gl.names();
        assert!(!names.contains(&"createFramebuffer".to_string()));
        assert!(!names.contains(&"createProgram".to_string()));

        // A resize reallocates them (old ones freed first).
        gl.clear();
        backend.set_backing_size(120.0, 120.0);
        backend.render(&scene, &coords, &theme());
        let ops = gl.ops();
        let del = ops
            .iter()
            .position(|op| op[0] == "deleteFramebuffer")
            .unwrap();
        assert_eq!(ops[del], json!(["deleteFramebuffer", "framebuffer#1"]));
        assert_eq!(ops[del + 1], json!(["deleteFramebuffer", "framebuffer#2"]));
        assert_eq!(ops[del + 2], json!(["deleteTexture", "texture#1"]));
        assert_eq!(ops[del + 3], json!(["deleteTexture", "texture#2"]));
        assert!(ops.contains(&json!([
            "texImage2D",
            3553,
            0,
            6408,
            60,
            60,
            0,
            6408,
            5121,
            Value::Null
        ])));
    }

    #[test]
    fn glow_sources_are_circles_crosses_steps_and_forced_overlays_but_not_fills_or_bars() {
        let mut scene = scene3();
        let mut m = model("ind", "price");
        let mut forced = line_like("f", LineLikeKind::Circles, &vals(&[20.0]), "#ff0000", 2.0);
        forced.overlay = true;
        m.series = vec![
            line_like(
                "s",
                LineLikeKind::Step,
                &vals(&[20.0, 30.0]),
                "#00ff00",
                1.0,
            ),
            line_like("c", LineLikeKind::Cross, &vals(&[20.0]), "#0000ff", 1.0),
            line_like("col", LineLikeKind::Columns, &vals(&[20.0]), "#ffffff", 1.0),
            candle_series(CandleKind::Candle),
            forced,
        ];
        add_model(&mut scene, m);
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        backend.glow = 1.0;
        backend.render(&scene, &coords, &theme());
        let uploads: Vec<Value> = gl.calls("bufferData");
        // main pass, the screen quad (built lazily), the glow-source pass
        assert_eq!(uploads.len(), 3);
        let (g, b, r) = (col("#00ff00"), col("#0000ff"), col("#ff0000"));
        let mut e = Batch::new();
        e.seg(60.0, y(20.0), 80.0, y(20.0), 1.0, g);
        e.seg(80.0, y(20.0), 80.0, y(30.0), 1.0, g);
        e.seg(x(0) - 1.5, y(20.0), x(0) + 1.5, y(20.0), 1.0, b);
        e.seg(x(0), y(20.0) - 1.5, x(0), y(20.0) + 1.5, 1.0, b);
        e.circle(x(0), y(20.0), 2.0, r, 16);
        assert_eq!(uploads[2][2], buffer(&e));
    }

    #[test]
    fn a_glow_pipeline_that_cannot_build_turns_glow_off_without_retrying() {
        let scene = scene3();
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        gl.fail_nth_link(2);
        backend.glow = 1.0;
        backend.render(&scene, &coords, &theme());
        assert_eq!(backend.glow, 0.0);
        assert!(gl.calls("createFramebuffer").is_empty());
        gl.clear();
        backend.render(&scene, &coords, &theme());
        assert!(!gl.names().contains(&"createProgram".to_string()));
    }

    #[test]
    fn glow_fades_with_the_model_alpha() {
        let mut scene = scene3();
        scene.candles_hidden = true;
        let mut m = model("ind", "price");
        m.series = vec![line_like(
            "l",
            LineLikeKind::Circles,
            &vals(&[20.0]),
            "#ff0000",
            2.0,
        )];
        add_model(&mut scene, m);
        let coords = coords3(&scene);
        let (gl, mut backend) = mounted();
        backend.glow = 1.0;
        backend.set_model_alpha(0.5);
        backend.render(&scene, &coords, &theme());
        let uploads = gl.calls("bufferData");
        let mut e = Batch::new();
        e.alpha = 0.5;
        e.circle(x(0), y(20.0), 2.0, col("#ff0000"), 16);
        assert_eq!(uploads[0][2], buffer(&e));
        assert_eq!(uploads[2][2], buffer(&e));
    }
}

/// The WebGL2 block of the original `native-close-styles.test.ts`: the close-only price
/// styles through `emit_price_series`, read back as bounding boxes of the batch's quads.
#[cfg(test)]
mod close_style_ports {
    use super::*;
    use crate::render::backend::canvas2d_backend::test_support::{coords_for, scene_with};

    const PANE_H: f64 = 100.0;
    const CLOSES: [f64; 5] = [10.0, 12.0, 11.0, 11.0, 13.0];
    const LINE: &str = "#123456";

    #[derive(Debug, PartialEq)]
    struct BoxOf {
        min_x: f64,
        max_x: f64,
        min_y: f64,
        max_y: f64,
        rgba: [f64; 4],
    }

    /// A 300 x 100 price pane over 0..20 (y = 100 - 5 * close); bars sit at x = 210 ... 290.
    fn scene_for(style: &str, spacing: f64) -> (SceneGraph, CoordinateSystem) {
        let bars: Vec<Ohlcv> = CLOSES
            .iter()
            .enumerate()
            .map(|(i, c)| Ohlcv::new(i as i64 * 60_000, *c, *c, *c, *c, None))
            .collect();
        let mut scene = scene_with(bars, 0.0, PANE_H, 0.0, 20.0);
        scene.price_style = style.into();
        scene.style.line.color = Some(LINE.into());
        let coords = coords_for(&scene, 300.0, PANE_H, 1.0, spacing, 0.5);
        (scene, coords)
    }

    fn y_of(close: f64) -> f64 {
        PANE_H - 5.0 * close
    }

    fn emit(style: &str, spacing: f64) -> Batch {
        let (scene, coords) = scene_for(style, spacing);
        let theme = serde_json::json!({
            "background": "#000000",
            "textColor": "#cccccc",
            "gridColor": "#222222",
            "borderColor": "#333333",
            "upColor": "#00aa00",
            "downColor": "#aa0000",
            "fontFamily": "sans-serif",
        });
        let plane = Plane::of(scene.panes.get("price").unwrap());
        let backend = WebGL2Backend::new();
        let mut b = Batch::new();
        backend.emit_price_series(
            &mut b,
            &scene,
            0,
            CLOSES.len() - 1,
            &coords,
            &plane,
            &theme,
            &HashMap::new(),
            300.0,
        );
        b
    }

    fn quads(b: &Batch) -> Vec<BoxOf> {
        let v = b.view();
        let mut out = Vec::new();
        let mut q = 0;
        while q + 48 <= v.len() {
            let (mut xs, mut ys) = (Vec::new(), Vec::new());
            for k in 0..6 {
                xs.push(f64::from(v[q + k * 8]));
                ys.push(f64::from(v[q + k * 8 + 1]));
            }
            let fold = |vals: &[f64], pick: fn(f64, f64) -> f64| {
                vals.iter().copied().reduce(pick).unwrap_or(f64::NAN)
            };
            let mut rgba = [0.0; 4];
            for (slot, c) in rgba.iter_mut().zip(&v[q + 2..q + 6]) {
                *slot = (f64::from(*c) * 1000.0).round() / 1000.0;
            }
            out.push(BoxOf {
                min_x: fold(&xs, f64::min),
                max_x: fold(&xs, f64::max),
                min_y: fold(&ys, f64::min),
                max_y: fold(&ys, f64::max),
                rgba,
            });
            q += 48;
        }
        out
    }

    #[test]
    fn columns_one_rect_per_bar_down_to_the_pane_bottom_in_the_same_slots_and_colors_as_canvas2d() {
        let rects = quads(&emit("columns", 20.0));
        // The original's test stubs its color parser (exact 0.667 / 0.5); the real one
        // reads back 8-bit premultiplied channels (170 / 255 at alpha 128 / 255).
        let up = [0.0, 0.663, 0.0, 0.502];
        let down = [0.663, 0.0, 0.0, 0.502];
        assert_eq!(
            rects.iter().map(|r| r.rgba).collect::<Vec<_>>(),
            vec![up, up, down, up, up]
        );
        assert_eq!(
            rects.iter().map(|r| r.min_y).collect::<Vec<_>>(),
            CLOSES.iter().map(|c| y_of(*c)).collect::<Vec<_>>()
        );
        for r in &rects {
            assert_eq!(r.max_y, PANE_H);
        }
        assert_eq!(
            rects.iter().map(|r| (r.min_x, r.max_x)).collect::<Vec<_>>(),
            vec![
                (200.0, 219.0),
                (220.0, 239.0),
                (240.0, 259.0),
                (260.0, 279.0),
                (280.0, 299.0)
            ]
        );
    }

    #[test]
    fn line_with_markers_a_filled_dot_per_bar_when_spaced_none_when_dense() {
        // A dot is a triangle fan whose center vertices carry edge distance 0 and
        // half-width = its radius.
        let dots = |b: &Batch| -> usize {
            let v = b.view();
            let mut centers = std::collections::HashSet::new();
            let r = line_marker_radius(2.0) as f32;
            for k in (0..v.len()).step_by(8) {
                if v[k + 6] == 0.0 && v[k + 7] == r {
                    centers.insert((v[k].to_bits(), v[k + 1].to_bits()));
                }
            }
            centers.len()
        };
        assert_eq!(dots(&emit("linemarkers", 20.0)), 5);
        assert_eq!(dots(&emit("linemarkers", 6.0)), 0);
        assert_eq!(dots(&emit("line", 20.0)), 0);
    }

    #[test]
    fn step_line_axis_aligned_rects_that_cover_the_steps_without_overlapping() {
        let rects = quads(&emit("stepline", 20.0));
        // Half-width 1: the first run is capped at its start, each rect owns the corner
        // at its end.
        let got: Vec<[f64; 4]> = rects
            .iter()
            .map(|r| [r.min_x, r.max_x, r.min_y, r.max_y])
            .collect();
        assert_eq!(
            got,
            vec![
                [209.0, 231.0, 49.0, 51.0], // 210 -> 230 at y 50
                [229.0, 231.0, 39.0, 49.0], // up to 40
                [231.0, 251.0, 39.0, 41.0], // 230 -> 250 at y 40
                [249.0, 251.0, 41.0, 46.0], // down to 45
                [251.0, 271.0, 44.0, 46.0], // 250 -> 270 at y 45
                [271.0, 291.0, 44.0, 46.0], // 270 -> 290 (unchanged close)
                [289.0, 291.0, 34.0, 44.0], // up to 35, square end cap
            ]
        );
        // No two rects share area: a translucent line keeps one tone through its corners.
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (a, c) = (&rects[i], &rects[j]);
                let area = f64::max(0.0, f64::min(a.max_x, c.max_x) - f64::max(a.min_x, c.min_x))
                    * f64::max(0.0, f64::min(a.max_y, c.max_y) - f64::max(a.min_y, c.min_y));
                assert_eq!(area, 0.0, "rects {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn a_barcolor_tints_its_column() {
        let (mut scene, coords) = scene_for("columns", 20.0);
        let mut m = crate::render::backend::canvas2d_backend::test_support::model("ind", "price");
        m.bar_colors = vec![crate::model::BarColor {
            time: 2 * 60_000,
            color: "#abcdef".into(),
        }];
        scene.indicators.insert("ind", m);
        let colors = merge_bar_colors(scene.indicators.values());
        let theme = serde_json::json!({ "upColor": "#00aa00", "downColor": "#aa0000" });
        let plane = Plane::of(scene.panes.get("price").unwrap());
        let backend = WebGL2Backend::new();
        let mut b = Batch::new();
        backend.emit_price_series(
            &mut b, &scene, 0, 4, &coords, &plane, &theme, &colors, 300.0,
        );
        let rects = quads(&b);
        let tint = [0.671, 0.804, 0.937, 1.0];
        let up = [0.0, 0.663, 0.0, 0.502];
        assert_eq!(
            rects.iter().map(|r| r.rgba).collect::<Vec<_>>(),
            vec![up, up, tint, up, up]
        );
    }
}
