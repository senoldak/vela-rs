//! The WebGL2 context the geometry backend draws with.
//!
//! The original's `WebGL2Backend` paints through the browser's
//! `WebGL2RenderingContext`. Core stays platform-neutral, so the backend takes a
//! [`Gl`] instead: the subset of that API the backend uses, method for method,
//! with the same argument order and the real GL numeric constants ([`consts`]).
//! `vela-web` implements it on a real context; [`RecordingGl`] records every call,
//! which is what tests assert on and what the parity check against the original
//! compares (the original is run on a JavaScript context stub that records the
//! same way).
//!
//! Like [`Canvas2d`](crate::render::canvas::Canvas2d), methods take `&self`: the
//! context is shared (`Rc<dyn Gl>`).
//!
//! # Handles
//!
//! GL objects are opaque `Copy` handles ([`Shader`], [`Program`], [`Buffer`],
//! [`VertexArray`], [`Texture`], [`Framebuffer`], [`UniformLocation`]) wrapping a
//! `u32` the implementation allocates. A browser implementation keeps a table
//! from that id to the JavaScript object; ids are never reused for a live object.
//! A `create_*` that the browser answers with `null` returns `None`.
//!
//! # Argument types
//!
//! JavaScript numbers are doubles, and a float uniform or clear color is narrowed
//! by the driver, so those take `f64` as the original passes them (the recording
//! shows the double). Integer parameters (sizes, enums, locations) are `i32` /
//! `u32`. `pixelStorei` takes the boolean the original passes. Buffer uploads take
//! `&[f32]` (the original uploads `Float32Array`s only).
//!
//! # The recording
//!
//! [`RecordingGl`] records each call as the JSON array `[name, arg…]` under the
//! JavaScript member name (`bufferData`, `vertexAttribPointer`, …). Rules a
//! JavaScript recorder has to follow to produce the same stream:
//!
//! - Numbers as numbers (enums as their numeric value), booleans as booleans.
//! - A handle is the string `"<kind>#<n>"` (`shader`, `program`, `buffer`,
//!   `vertexArray`, `texture`, `framebuffer`, `uniform`) where `n` counts the
//!   objects of that kind created so far, from 1 (a uniform location is created by
//!   each `getUniformLocation` call). A null handle is `null`.
//! - A `Float32Array` upload is recorded IN FULL as an array of numbers (each
//!   float widened to a double, which is exact): tests build small scenes, and the
//!   full stream is what makes them readable. For the large scenes of the differential
//!   check, [`RecordingGl::digest_uploads_above`] switches an upload of more than `n`
//!   floats to the digest object `{"f32": len, "nz": n, "h1": u32, "h2": u32}`: `h1` is
//!   the 32-bit FNV-1a (offset 2166136261, prime 16777619) over the little-endian bytes
//!   of each float in order, `h2` the same with offset 0x9747b28c over the floats in
//!   REVERSE order. Any NaN counts as the bits 0x7fc00000 (a JavaScript engine and the
//!   CPU disagree on a NaN's sign and payload, which carry no meaning) and a negative
//!   zero as +0 (`nz` counts the negative zeros, so a recorder can still report them).
//! - Shader sources are recorded verbatim.
//! - `texImage2D` from a canvas records the source as the string `"canvas"`; with
//!   no pixel data the last argument is `null`.
//! - `getExtension` is recorded with the extension name, and `loseContext` as its
//!   own call right after it.
//! - Queries (`getShaderParameter`, `getProgramParameter`, `isContextLost`) are
//!   recorded like any other call, before their result is used.

use std::cell::{Cell, RefCell};

use serde_json::{Value, json};

use crate::render::canvas::CanvasImage;

/// The GL constants the backend uses, with their real values.
#[allow(missing_docs)]
pub mod consts {
    pub const ARRAY_BUFFER: u32 = 0x8892;
    pub const STATIC_DRAW: u32 = 0x88E4;
    pub const DYNAMIC_DRAW: u32 = 0x88E8;

    pub const TRIANGLES: u32 = 0x0004;
    pub const TRIANGLE_STRIP: u32 = 0x0005;
    pub const FLOAT: u32 = 0x1406;
    pub const UNSIGNED_BYTE: u32 = 0x1401;

    pub const VERTEX_SHADER: u32 = 0x8B31;
    pub const FRAGMENT_SHADER: u32 = 0x8B30;
    pub const COMPILE_STATUS: u32 = 0x8B81;
    pub const LINK_STATUS: u32 = 0x8B82;

    pub const BLEND: u32 = 0x0BE2;
    pub const SCISSOR_TEST: u32 = 0x0C11;
    pub const COLOR_BUFFER_BIT: u32 = 0x4000;
    pub const ZERO: u32 = 0;
    pub const ONE: u32 = 1;
    pub const SRC_ALPHA: u32 = 0x0302;
    pub const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;

    pub const TEXTURE_2D: u32 = 0x0DE1;
    pub const TEXTURE0: u32 = 0x84C0;
    pub const RGBA: u32 = 0x1908;
    pub const TEXTURE_MIN_FILTER: u32 = 0x2801;
    pub const TEXTURE_MAG_FILTER: u32 = 0x2800;
    pub const TEXTURE_WRAP_S: u32 = 0x2802;
    pub const TEXTURE_WRAP_T: u32 = 0x2803;
    pub const LINEAR: u32 = 0x2601;
    pub const CLAMP_TO_EDGE: u32 = 0x812F;

    pub const FRAMEBUFFER: u32 = 0x8D40;
    pub const COLOR_ATTACHMENT0: u32 = 0x8CE0;

    pub const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
    pub const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
}

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(pub u32);
    };
}

handle!(
    /// `WebGLShader`.
    Shader
);
handle!(
    /// `WebGLProgram`.
    Program
);
handle!(
    /// `WebGLBuffer`.
    Buffer
);
handle!(
    /// `WebGLVertexArrayObject`.
    VertexArray
);
handle!(
    /// `WebGLTexture`.
    Texture
);
handle!(
    /// `WebGLFramebuffer`.
    Framebuffer
);
handle!(
    /// `WebGLUniformLocation`.
    UniformLocation
);

/// The subset of `WebGL2RenderingContext` the WebGL2 backend uses. Every method is
/// the context member of the same name (snake_cased).
pub trait Gl {
    // ── state ──
    fn viewport(&self, x: i32, y: i32, width: i32, height: i32);
    fn scissor(&self, x: i32, y: i32, width: i32, height: i32);
    fn enable(&self, cap: u32);
    fn disable(&self, cap: u32);
    fn clear_color(&self, r: f64, g: f64, b: f64, a: f64);
    fn clear(&self, mask: u32);
    fn blend_func(&self, sfactor: u32, dfactor: u32);
    fn blend_func_separate(&self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32);

    // ── shaders and programs ──
    fn create_shader(&self, kind: u32) -> Option<Shader>;
    fn shader_source(&self, shader: Shader, source: &str);
    fn compile_shader(&self, shader: Shader);
    /// `getShaderParameter` for a boolean parameter (`COMPILE_STATUS`).
    fn get_shader_parameter(&self, shader: Shader, pname: u32) -> bool;
    fn delete_shader(&self, shader: Shader);
    fn create_program(&self) -> Option<Program>;
    fn attach_shader(&self, program: Program, shader: Shader);
    fn link_program(&self, program: Program);
    /// `getProgramParameter` for a boolean parameter (`LINK_STATUS`).
    fn get_program_parameter(&self, program: Program, pname: u32) -> bool;
    fn delete_program(&self, program: Program);
    fn use_program(&self, program: Option<Program>);
    fn get_uniform_location(&self, program: Program, name: &str) -> Option<UniformLocation>;
    fn uniform1i(&self, location: Option<UniformLocation>, x: i32);
    fn uniform1f(&self, location: Option<UniformLocation>, x: f64);
    fn uniform2f(&self, location: Option<UniformLocation>, x: f64, y: f64);

    // ── buffers and vertex arrays ──
    fn create_buffer(&self) -> Option<Buffer>;
    fn delete_buffer(&self, buffer: Buffer);
    fn bind_buffer(&self, target: u32, buffer: Option<Buffer>);
    /// `bufferData(target, Float32Array, usage)`.
    fn buffer_data(&self, target: u32, data: &[f32], usage: u32);
    fn create_vertex_array(&self) -> Option<VertexArray>;
    fn delete_vertex_array(&self, vao: VertexArray);
    fn bind_vertex_array(&self, vao: Option<VertexArray>);
    fn enable_vertex_attrib_array(&self, index: u32);
    fn vertex_attrib_pointer(
        &self,
        index: u32,
        size: i32,
        kind: u32,
        normalized: bool,
        stride: i32,
        offset: i32,
    );
    fn draw_arrays(&self, mode: u32, first: i32, count: i32);

    // ── textures and framebuffers ──
    fn create_texture(&self) -> Option<Texture>;
    fn delete_texture(&self, texture: Texture);
    fn active_texture(&self, texture: u32);
    fn bind_texture(&self, target: u32, texture: Option<Texture>);
    /// `texImage2D(target, level, internalformat, width, height, border, format,
    /// type, null)`: allocate an empty level.
    #[allow(clippy::too_many_arguments)]
    fn tex_image_2d_empty(
        &self,
        target: u32,
        level: i32,
        internal_format: u32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        kind: u32,
    );
    /// `texImage2D(target, level, internalformat, format, type, canvas)`: upload a
    /// canvas's pixels (subject to the `pixelStorei` flags).
    fn tex_image_2d_canvas(
        &self,
        target: u32,
        level: i32,
        internal_format: u32,
        format: u32,
        kind: u32,
        source: &CanvasImage,
    );
    fn tex_parameteri(&self, target: u32, pname: u32, param: i32);
    fn pixel_storei(&self, pname: u32, param: bool);
    fn create_framebuffer(&self) -> Option<Framebuffer>;
    fn delete_framebuffer(&self, framebuffer: Framebuffer);
    fn bind_framebuffer(&self, target: u32, framebuffer: Option<Framebuffer>);
    fn framebuffer_texture_2d(
        &self,
        target: u32,
        attachment: u32,
        tex_target: u32,
        texture: Option<Texture>,
        level: i32,
    );

    // ── context ──
    fn is_context_lost(&self) -> bool;
    /// `getExtension('WEBGL_lose_context')?.loseContext()`: release the context
    /// itself (browsers cap live contexts per page). A no-op when the extension is
    /// missing.
    fn lose_context(&self);
}

/// A [`Gl`] that draws nothing and records every call as `[name, arg…]` (see the
/// module documentation for the format).
///
/// Failure knobs let tests drive the backend's fallback paths: a compile or link
/// that reports failure ([`set_compile_ok`](Self::set_compile_ok),
/// [`set_link_ok`](Self::set_link_ok), [`fail_nth_link`](Self::fail_nth_link)),
/// a context that reports itself lost ([`set_context_lost`](Self::set_context_lost)),
/// and creators that return `None` ([`set_create_ok`](Self::set_create_ok)).
pub struct RecordingGl {
    ops: RefCell<Vec<Value>>,
    counters: RefCell<[u32; 7]>,
    compile_ok: Cell<bool>,
    link_ok: Cell<bool>,
    links: Cell<u32>,
    fail_link_at: Cell<Option<u32>>,
    create_ok: Cell<bool>,
    lost: Cell<bool>,
    digest_above: Cell<Option<usize>>,
}

impl Default for RecordingGl {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Shader,
    Program,
    Buffer,
    VertexArray,
    Texture,
    Framebuffer,
    Uniform,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Shader => "shader",
            Kind::Program => "program",
            Kind::Buffer => "buffer",
            Kind::VertexArray => "vertexArray",
            Kind::Texture => "texture",
            Kind::Framebuffer => "framebuffer",
            Kind::Uniform => "uniform",
        }
    }
}

fn handle_json(kind: Kind, id: Option<u32>) -> Value {
    match id {
        Some(n) => json!(format!("{}#{n}", kind.name())),
        None => Value::Null,
    }
}

impl RecordingGl {
    pub fn new() -> Self {
        Self {
            ops: RefCell::new(Vec::new()),
            counters: RefCell::new([0; 7]),
            compile_ok: Cell::new(true),
            link_ok: Cell::new(true),
            links: Cell::new(0),
            fail_link_at: Cell::new(None),
            create_ok: Cell::new(true),
            lost: Cell::new(false),
            digest_above: Cell::new(None),
        }
    }

    fn push(&self, op: Value) {
        self.ops.borrow_mut().push(op);
    }

    fn next_id(&self, kind: Kind) -> u32 {
        let mut counters = self.counters.borrow_mut();
        let slot = &mut counters[kind as usize];
        *slot += 1;
        *slot
    }

    /// Allocate a handle for a `create_*` call and record it. `None` when creators
    /// are set to fail (the call is still recorded).
    fn create(&self, name: &str, kind: Kind, args: &[Value]) -> Option<u32> {
        let id = self.create_ok.get().then(|| self.next_id(kind));
        let mut op = vec![json!(name)];
        op.extend_from_slice(args);
        self.push(Value::Array(op));
        id
    }

    /// Every recorded call so far, in order.
    pub fn ops(&self) -> Vec<Value> {
        self.ops.borrow().clone()
    }

    /// The recorded calls as one JSON array.
    pub fn to_json(&self) -> Value {
        Value::Array(self.ops())
    }

    /// The names of the recorded calls, in order.
    pub fn names(&self) -> Vec<String> {
        self.ops
            .borrow()
            .iter()
            .filter_map(|op| op.get(0).and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    /// The recorded calls named `name`.
    pub fn calls(&self, name: &str) -> Vec<Value> {
        self.ops
            .borrow()
            .iter()
            .filter(|op| op.get(0).and_then(Value::as_str) == Some(name))
            .cloned()
            .collect()
    }

    /// Forget what was recorded (object counters and knobs are kept).
    pub fn clear(&self) {
        self.ops.borrow_mut().clear();
    }

    pub fn is_empty(&self) -> bool {
        self.ops.borrow().is_empty()
    }

    /// Whether `compile_shader` succeeds (default true).
    pub fn set_compile_ok(&self, ok: bool) {
        self.compile_ok.set(ok);
    }

    /// Whether `link_program` succeeds (default true).
    pub fn set_link_ok(&self, ok: bool) {
        self.link_ok.set(ok);
    }

    /// Make the `n`th `link_program` call from now on (1-based, counted across the
    /// recording's lifetime) fail, and no other.
    pub fn fail_nth_link(&self, n: u32) {
        self.fail_link_at.set(Some(n));
    }

    /// Whether `create_*` return an object (default true).
    pub fn set_create_ok(&self, ok: bool) {
        self.create_ok.set(ok);
    }

    /// Record a buffer upload of more than `limit` floats as a digest instead of in full
    /// (see the module documentation); `None` records every upload in full (the default).
    pub fn digest_uploads_above(&self, limit: Option<usize>) {
        self.digest_above.set(limit);
    }

    /// What `is_context_lost` answers (default false).
    pub fn set_context_lost(&self, lost: bool) {
        self.lost.set(lost);
    }
}

/// A float's bits as the digest sees them: any NaN is the canonical quiet NaN, a negative
/// zero is zero.
fn digest_bits(v: f32) -> u32 {
    if v.is_nan() {
        0x7fc0_0000
    } else if v == 0.0 {
        0
    } else {
        v.to_bits()
    }
}

fn fnv_word(mut h: u32, word: u32) -> u32 {
    for byte in word.to_le_bytes() {
        h ^= u32::from(byte);
        h = h.wrapping_mul(16_777_619);
    }
    h
}

/// The digest of an upload (see the module documentation).
fn digest_json(data: &[f32]) -> Value {
    let h1 = data
        .iter()
        .fold(2_166_136_261, |h, v| fnv_word(h, digest_bits(*v)));
    let h2 = data
        .iter()
        .rev()
        .fold(0x9747_b28c, |h, v| fnv_word(h, digest_bits(*v)));
    let negative_zeros = data
        .iter()
        .filter(|v| **v == 0.0 && v.is_sign_negative())
        .count();
    json!({ "f32": data.len(), "nz": negative_zeros, "h1": h1, "h2": h2 })
}

fn opt<T>(handle: Option<T>, kind: Kind, id: impl Fn(&T) -> u32) -> Value {
    handle_json(kind, handle.as_ref().map(id))
}

impl Gl for RecordingGl {
    fn viewport(&self, x: i32, y: i32, width: i32, height: i32) {
        self.push(json!(["viewport", x, y, width, height]));
    }

    fn scissor(&self, x: i32, y: i32, width: i32, height: i32) {
        self.push(json!(["scissor", x, y, width, height]));
    }

    fn enable(&self, cap: u32) {
        self.push(json!(["enable", cap]));
    }

    fn disable(&self, cap: u32) {
        self.push(json!(["disable", cap]));
    }

    fn clear_color(&self, r: f64, g: f64, b: f64, a: f64) {
        self.push(json!(["clearColor", r, g, b, a]));
    }

    fn clear(&self, mask: u32) {
        self.push(json!(["clear", mask]));
    }

    fn blend_func(&self, sfactor: u32, dfactor: u32) {
        self.push(json!(["blendFunc", sfactor, dfactor]));
    }

    fn blend_func_separate(&self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
        self.push(json!([
            "blendFuncSeparate",
            src_rgb,
            dst_rgb,
            src_alpha,
            dst_alpha
        ]));
    }

    fn create_shader(&self, kind: u32) -> Option<Shader> {
        self.create("createShader", Kind::Shader, &[json!(kind)])
            .map(Shader)
    }

    fn shader_source(&self, shader: Shader, source: &str) {
        self.push(json!([
            "shaderSource",
            handle_json(Kind::Shader, Some(shader.0)),
            source
        ]));
    }

    fn compile_shader(&self, shader: Shader) {
        self.push(json!([
            "compileShader",
            handle_json(Kind::Shader, Some(shader.0))
        ]));
    }

    fn get_shader_parameter(&self, shader: Shader, pname: u32) -> bool {
        self.push(json!([
            "getShaderParameter",
            handle_json(Kind::Shader, Some(shader.0)),
            pname
        ]));
        self.compile_ok.get()
    }

    fn delete_shader(&self, shader: Shader) {
        self.push(json!([
            "deleteShader",
            handle_json(Kind::Shader, Some(shader.0))
        ]));
    }

    fn create_program(&self) -> Option<Program> {
        self.create("createProgram", Kind::Program, &[])
            .map(Program)
    }

    fn attach_shader(&self, program: Program, shader: Shader) {
        self.push(json!([
            "attachShader",
            handle_json(Kind::Program, Some(program.0)),
            handle_json(Kind::Shader, Some(shader.0))
        ]));
    }

    fn link_program(&self, program: Program) {
        self.links.set(self.links.get() + 1);
        self.push(json!([
            "linkProgram",
            handle_json(Kind::Program, Some(program.0))
        ]));
    }

    fn get_program_parameter(&self, program: Program, pname: u32) -> bool {
        self.push(json!([
            "getProgramParameter",
            handle_json(Kind::Program, Some(program.0)),
            pname
        ]));
        self.link_ok.get() && self.fail_link_at.get() != Some(self.links.get())
    }

    fn delete_program(&self, program: Program) {
        self.push(json!([
            "deleteProgram",
            handle_json(Kind::Program, Some(program.0))
        ]));
    }

    fn use_program(&self, program: Option<Program>) {
        self.push(json!(["useProgram", opt(program, Kind::Program, |p| p.0)]));
    }

    fn get_uniform_location(&self, program: Program, name: &str) -> Option<UniformLocation> {
        self.create(
            "getUniformLocation",
            Kind::Uniform,
            &[handle_json(Kind::Program, Some(program.0)), json!(name)],
        )
        .map(UniformLocation)
    }

    fn uniform1i(&self, location: Option<UniformLocation>, x: i32) {
        self.push(json!([
            "uniform1i",
            opt(location, Kind::Uniform, |l| l.0),
            x
        ]));
    }

    fn uniform1f(&self, location: Option<UniformLocation>, x: f64) {
        self.push(json!([
            "uniform1f",
            opt(location, Kind::Uniform, |l| l.0),
            x
        ]));
    }

    fn uniform2f(&self, location: Option<UniformLocation>, x: f64, y: f64) {
        self.push(json!([
            "uniform2f",
            opt(location, Kind::Uniform, |l| l.0),
            x,
            y
        ]));
    }

    fn create_buffer(&self) -> Option<Buffer> {
        self.create("createBuffer", Kind::Buffer, &[]).map(Buffer)
    }

    fn delete_buffer(&self, buffer: Buffer) {
        self.push(json!([
            "deleteBuffer",
            handle_json(Kind::Buffer, Some(buffer.0))
        ]));
    }

    fn bind_buffer(&self, target: u32, buffer: Option<Buffer>) {
        self.push(json!([
            "bindBuffer",
            target,
            opt(buffer, Kind::Buffer, |b| b.0)
        ]));
    }

    fn buffer_data(&self, target: u32, data: &[f32], usage: u32) {
        let recorded = match self.digest_above.get() {
            Some(limit) if data.len() > limit => digest_json(data),
            _ => Value::Array(data.iter().map(|v| json!(f64::from(*v))).collect()),
        };
        self.push(json!(["bufferData", target, recorded, usage]));
    }

    fn create_vertex_array(&self) -> Option<VertexArray> {
        self.create("createVertexArray", Kind::VertexArray, &[])
            .map(VertexArray)
    }

    fn delete_vertex_array(&self, vao: VertexArray) {
        self.push(json!([
            "deleteVertexArray",
            handle_json(Kind::VertexArray, Some(vao.0))
        ]));
    }

    fn bind_vertex_array(&self, vao: Option<VertexArray>) {
        self.push(json!([
            "bindVertexArray",
            opt(vao, Kind::VertexArray, |v| v.0)
        ]));
    }

    fn enable_vertex_attrib_array(&self, index: u32) {
        self.push(json!(["enableVertexAttribArray", index]));
    }

    fn vertex_attrib_pointer(
        &self,
        index: u32,
        size: i32,
        kind: u32,
        normalized: bool,
        stride: i32,
        offset: i32,
    ) {
        self.push(json!([
            "vertexAttribPointer",
            index,
            size,
            kind,
            normalized,
            stride,
            offset
        ]));
    }

    fn draw_arrays(&self, mode: u32, first: i32, count: i32) {
        self.push(json!(["drawArrays", mode, first, count]));
    }

    fn create_texture(&self) -> Option<Texture> {
        self.create("createTexture", Kind::Texture, &[])
            .map(Texture)
    }

    fn delete_texture(&self, texture: Texture) {
        self.push(json!([
            "deleteTexture",
            handle_json(Kind::Texture, Some(texture.0))
        ]));
    }

    fn active_texture(&self, texture: u32) {
        self.push(json!(["activeTexture", texture]));
    }

    fn bind_texture(&self, target: u32, texture: Option<Texture>) {
        self.push(json!([
            "bindTexture",
            target,
            opt(texture, Kind::Texture, |t| t.0)
        ]));
    }

    fn tex_image_2d_empty(
        &self,
        target: u32,
        level: i32,
        internal_format: u32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        kind: u32,
    ) {
        self.push(json!([
            "texImage2D",
            target,
            level,
            internal_format,
            width,
            height,
            border,
            format,
            kind,
            Value::Null
        ]));
    }

    fn tex_image_2d_canvas(
        &self,
        target: u32,
        level: i32,
        internal_format: u32,
        format: u32,
        kind: u32,
        _source: &CanvasImage,
    ) {
        self.push(json!([
            "texImage2D",
            target,
            level,
            internal_format,
            format,
            kind,
            "canvas"
        ]));
    }

    fn tex_parameteri(&self, target: u32, pname: u32, param: i32) {
        self.push(json!(["texParameteri", target, pname, param]));
    }

    fn pixel_storei(&self, pname: u32, param: bool) {
        self.push(json!(["pixelStorei", pname, param]));
    }

    fn create_framebuffer(&self) -> Option<Framebuffer> {
        self.create("createFramebuffer", Kind::Framebuffer, &[])
            .map(Framebuffer)
    }

    fn delete_framebuffer(&self, framebuffer: Framebuffer) {
        self.push(json!([
            "deleteFramebuffer",
            handle_json(Kind::Framebuffer, Some(framebuffer.0))
        ]));
    }

    fn bind_framebuffer(&self, target: u32, framebuffer: Option<Framebuffer>) {
        self.push(json!([
            "bindFramebuffer",
            target,
            opt(framebuffer, Kind::Framebuffer, |f| f.0)
        ]));
    }

    fn framebuffer_texture_2d(
        &self,
        target: u32,
        attachment: u32,
        tex_target: u32,
        texture: Option<Texture>,
        level: i32,
    ) {
        self.push(json!([
            "framebufferTexture2D",
            target,
            attachment,
            tex_target,
            opt(texture, Kind::Texture, |t| t.0),
            level
        ]));
    }

    fn is_context_lost(&self) -> bool {
        self.push(json!(["isContextLost"]));
        self.lost.get()
    }

    fn lose_context(&self) {
        self.push(json!(["getExtension", "WEBGL_lose_context"]));
        self.push(json!(["loseContext"]));
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    #[test]
    fn constants_carry_the_real_values() {
        assert_eq!(consts::ARRAY_BUFFER, 34962);
        assert_eq!(consts::DYNAMIC_DRAW, 35048);
        assert_eq!(consts::STATIC_DRAW, 35044);
        assert_eq!(consts::TRIANGLES, 4);
        assert_eq!(consts::TRIANGLE_STRIP, 5);
        assert_eq!(consts::FLOAT, 5126);
        assert_eq!(consts::UNSIGNED_BYTE, 5121);
        assert_eq!(consts::VERTEX_SHADER, 35633);
        assert_eq!(consts::FRAGMENT_SHADER, 35632);
        assert_eq!(consts::COMPILE_STATUS, 35713);
        assert_eq!(consts::LINK_STATUS, 35714);
        assert_eq!(consts::BLEND, 3042);
        assert_eq!(consts::SCISSOR_TEST, 3089);
        assert_eq!(consts::COLOR_BUFFER_BIT, 16384);
        assert_eq!(consts::SRC_ALPHA, 770);
        assert_eq!(consts::ONE_MINUS_SRC_ALPHA, 771);
        assert_eq!(consts::TEXTURE_2D, 3553);
        assert_eq!(consts::TEXTURE0, 33984);
        assert_eq!(consts::RGBA, 6408);
        assert_eq!(consts::LINEAR, 9729);
        assert_eq!(consts::CLAMP_TO_EDGE, 33071);
        assert_eq!(consts::FRAMEBUFFER, 36160);
        assert_eq!(consts::COLOR_ATTACHMENT0, 36064);
        assert_eq!(consts::UNPACK_FLIP_Y_WEBGL, 37440);
        assert_eq!(consts::UNPACK_PREMULTIPLY_ALPHA_WEBGL, 37441);
    }

    #[test]
    fn it_records_calls_with_counted_handles() {
        let gl = RecordingGl::new();
        let vs = gl.create_shader(consts::VERTEX_SHADER).unwrap();
        gl.shader_source(vs, "src");
        gl.compile_shader(vs);
        assert!(gl.get_shader_parameter(vs, consts::COMPILE_STATUS));
        let p = gl.create_program().unwrap();
        gl.attach_shader(p, vs);
        gl.link_program(p);
        assert!(gl.get_program_parameter(p, consts::LINK_STATUS));
        let loc = gl.get_uniform_location(p, "uRes");
        gl.uniform2f(loc, 1.5, 2.0);
        gl.use_program(None);
        gl.buffer_data(consts::ARRAY_BUFFER, &[0.5, 1.0], consts::DYNAMIC_DRAW);
        let expected = json!([
            ["createShader", 35633],
            ["shaderSource", "shader#1", "src"],
            ["compileShader", "shader#1"],
            ["getShaderParameter", "shader#1", 35713],
            ["createProgram"],
            ["attachShader", "program#1", "shader#1"],
            ["linkProgram", "program#1"],
            ["getProgramParameter", "program#1", 35714],
            ["getUniformLocation", "program#1", "uRes"],
            ["uniform2f", "uniform#1", 1.5, 2.0],
            ["useProgram", null],
            ["bufferData", 34962, [0.5, 1.0], 35048],
        ]);
        assert_eq!(gl.to_json(), expected);
    }

    #[test]
    fn failure_knobs_change_the_answers() {
        let gl = RecordingGl::new();
        let s = gl.create_shader(consts::VERTEX_SHADER).unwrap();
        gl.set_compile_ok(false);
        assert!(!gl.get_shader_parameter(s, consts::COMPILE_STATUS));
        let p = gl.create_program().unwrap();
        gl.fail_nth_link(2);
        gl.link_program(p);
        assert!(gl.get_program_parameter(p, consts::LINK_STATUS));
        gl.link_program(p);
        assert!(!gl.get_program_parameter(p, consts::LINK_STATUS));
        gl.set_create_ok(false);
        assert!(gl.create_buffer().is_none());
        gl.set_context_lost(true);
        assert!(gl.is_context_lost());
    }

    #[test]
    fn large_uploads_can_be_recorded_as_a_digest() {
        let gl = RecordingGl::new();
        gl.digest_uploads_above(Some(2));
        gl.buffer_data(consts::ARRAY_BUFFER, &[1.0, 2.0], consts::DYNAMIC_DRAW);
        gl.buffer_data(consts::ARRAY_BUFFER, &[1.0, 2.0, 3.0], consts::DYNAMIC_DRAW);
        gl.buffer_data(
            consts::ARRAY_BUFFER,
            &[1.0, 2.0, f32::NAN],
            consts::DYNAMIC_DRAW,
        );
        let negative_nan = f32::from_bits(0xffc0_0000);
        gl.buffer_data(
            consts::ARRAY_BUFFER,
            &[1.0, 2.0, negative_nan],
            consts::DYNAMIC_DRAW,
        );
        let ops = gl.ops();
        assert_eq!(ops[0][2], json!([1.0, 2.0]), "short uploads stay in full");
        assert_eq!(ops[1][2]["f32"], json!(3));
        assert_ne!(ops[1][2], ops[2][2], "the digest tells values apart");
        assert_eq!(ops[2][2], ops[3][2], "every NaN digests alike");
        gl.buffer_data(consts::ARRAY_BUFFER, &[1.0, 0.0, 0.0], consts::DYNAMIC_DRAW);
        gl.buffer_data(
            consts::ARRAY_BUFFER,
            &[1.0, 0.0, -0.0],
            consts::DYNAMIC_DRAW,
        );
        let ops = gl.ops();
        assert_eq!(ops[4][2]["nz"], json!(0));
        assert_eq!(ops[5][2]["nz"], json!(1));
        assert_eq!(
            ops[4][2]["h1"], ops[5][2]["h1"],
            "a negative zero digests as zero"
        );
        // FNV-1a over the bytes of 1.0f32 = 00 00 80 3f, from the 32-bit offset.
        let mut h = 2_166_136_261u32;
        for b in [0x00u8, 0x00, 0x80, 0x3f] {
            h = (h ^ u32::from(b)).wrapping_mul(16_777_619);
        }
        assert_eq!(fnv_word(2_166_136_261, 1.0f32.to_bits()), h);
    }

    #[test]
    fn the_trait_is_object_safe() {
        let gl: Rc<dyn Gl> = Rc::new(RecordingGl::new());
        gl.lose_context();
    }
}
