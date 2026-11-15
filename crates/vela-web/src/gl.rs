//! [`vela_core::render::gl::Gl`] on a real `WebGL2RenderingContext`, and the data canvas that
//! carries it.
//!
//! The core's WebGL2 backend paints through the [`Gl`] trait, a method-for-method subset of
//! the context. [`BrowserGl`] forwards each call. GL objects are opaque `u32` handles on the
//! trait side; here an id table maps each handle to its JavaScript object (one table per
//! kind, ids never reused, an entry leaves the table when its object is deleted). A
//! `create_*` the browser answers with `null` returns `None`, and so does
//! `get_uniform_location`.
//!
//! Nothing here may abort a frame: the calls that can throw in a browser (a `texImage2D`
//! from a tainted canvas, a missing extension) are swallowed, and a handle with no object
//! behind it (deleted, or from before a context restore) is skipped like the no-op GL
//! itself makes of it.
//!
//! Real WebGL is only exercised in a browser; the Node smoke test (`smoke_gl.mjs`) drives
//! this file against a recording context stand-in.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Float32Array, Reflect};
use vela_core::options::NativeBackend;
use vela_core::render::backend::render_backend::BackendKind;
use vela_core::render::canvas::CanvasImage;
use vela_core::render::gl::{
    Buffer, Framebuffer, Gl, Program, Shader, Texture, UniformLocation, VertexArray,
};
use vela_core::render::native::HostGlCanvas;
use wasm_bindgen::prelude::*;
use web_sys::{
    HtmlCanvasElement, WebGl2RenderingContext, WebGlBuffer, WebGlFramebuffer, WebGlProgram,
    WebGlShader, WebGlTexture, WebGlUniformLocation, WebGlVertexArrayObject, WebglLoseContext,
};

use crate::dom::{Listener, set_styles};

#[wasm_bindgen(inline_js = r#"
// The context the original's `WebGL2Backend.mount` asks for. `null` when WebGL2 is not
// available (no support, blocked, or the canvas already has another kind of context).
export function vela_webgl2_context(canvas) {
    try {
        return canvas.getContext('webgl2', {
            alpha: true,
            antialias: false,
            depth: false,
            stencil: false,
            premultipliedAlpha: true,
        }) || null;
    } catch (_) {
        return null;
    }
}

// texImage2D from any image source (a canvas, an image element, a bitmap). A tainted or
// unusable source throws; the frame goes on without the texture update.
export function vela_tex_image_source(gl, target, level, internalFormat, format, type, source) {
    try {
        gl.texImage2D(target, level, internalFormat, format, type, source);
    } catch (_) {
        // Nothing to upload.
    }
}
"#)]
extern "C" {
    fn vela_webgl2_context(canvas: &HtmlCanvasElement) -> JsValue;
    fn vela_tex_image_source(
        gl: &WebGl2RenderingContext,
        target: u32,
        level: i32,
        internal_format: u32,
        format: u32,
        kind: u32,
        source: &JsValue,
    );
}

/// Open the `webgl2` context of `canvas`, `None` when the browser has none.
pub(crate) fn webgl2_context_of(canvas: &HtmlCanvasElement) -> Option<WebGl2RenderingContext> {
    let ctx = vela_webgl2_context(canvas);
    if ctx.is_null() || ctx.is_undefined() {
        None
    } else {
        Some(ctx.unchecked_into())
    }
}

/// The JavaScript value behind a [`CanvasImage`], whichever way the host wrapped it (a
/// `JsValue` for the slice canvases, the data canvas and the mark icons; a canvas element).
pub(crate) fn image_value(image: &CanvasImage) -> Option<JsValue> {
    let any: &dyn Any = image.as_ref();
    if let Some(value) = any.downcast_ref::<JsValue>() {
        return Some(value.clone());
    }
    any.downcast_ref::<HtmlCanvasElement>()
        .map(|canvas| JsValue::from(canvas.clone()))
}

#[derive(Default)]
struct Objects {
    next: u32,
    shaders: HashMap<u32, WebGlShader>,
    programs: HashMap<u32, WebGlProgram>,
    buffers: HashMap<u32, WebGlBuffer>,
    vertex_arrays: HashMap<u32, WebGlVertexArrayObject>,
    textures: HashMap<u32, WebGlTexture>,
    framebuffers: HashMap<u32, WebGlFramebuffer>,
    uniforms: HashMap<u32, WebGlUniformLocation>,
}

impl Objects {
    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
}

/// A browser WebGL2 context as the geometry backend's [`Gl`].
pub struct BrowserGl {
    ctx: WebGl2RenderingContext,
    objects: RefCell<Objects>,
}

impl BrowserGl {
    pub fn new(ctx: WebGl2RenderingContext) -> Self {
        Self {
            ctx,
            objects: RefCell::new(Objects::default()),
        }
    }

    /// The underlying context.
    pub fn context(&self) -> &WebGl2RenderingContext {
        &self.ctx
    }

    /// How many GL objects the table holds right now (all kinds).
    pub fn live_objects(&self) -> usize {
        let o = self.objects.borrow();
        o.shaders.len()
            + o.programs.len()
            + o.buffers.len()
            + o.vertex_arrays.len()
            + o.textures.len()
            + o.framebuffers.len()
            + o.uniforms.len()
    }
}

/// Insert `object` under a fresh id of the kind's table and return the id.
macro_rules! create {
    ($self:ident, $table:ident, $object:expr) => {{
        let object = $object?;
        let mut o = $self.objects.borrow_mut();
        let id = o.id();
        o.$table.insert(id, object);
        Some(id)
    }};
}

impl Gl for BrowserGl {
    // ── state ──
    fn viewport(&self, x: i32, y: i32, width: i32, height: i32) {
        self.ctx.viewport(x, y, width, height);
    }
    fn scissor(&self, x: i32, y: i32, width: i32, height: i32) {
        self.ctx.scissor(x, y, width, height);
    }
    fn enable(&self, cap: u32) {
        self.ctx.enable(cap);
    }
    fn disable(&self, cap: u32) {
        self.ctx.disable(cap);
    }
    fn clear_color(&self, r: f64, g: f64, b: f64, a: f64) {
        self.ctx.clear_color(r as f32, g as f32, b as f32, a as f32);
    }
    fn clear(&self, mask: u32) {
        self.ctx.clear(mask);
    }
    fn blend_func(&self, sfactor: u32, dfactor: u32) {
        self.ctx.blend_func(sfactor, dfactor);
    }
    fn blend_func_separate(&self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
        self.ctx
            .blend_func_separate(src_rgb, dst_rgb, src_alpha, dst_alpha);
    }

    // ── shaders and programs ──
    fn create_shader(&self, kind: u32) -> Option<Shader> {
        create!(self, shaders, self.ctx.create_shader(kind)).map(Shader)
    }
    fn shader_source(&self, shader: Shader, source: &str) {
        if let Some(s) = self.objects.borrow().shaders.get(&shader.0) {
            self.ctx.shader_source(s, source);
        }
    }
    fn compile_shader(&self, shader: Shader) {
        if let Some(s) = self.objects.borrow().shaders.get(&shader.0) {
            self.ctx.compile_shader(s);
        }
    }
    fn get_shader_parameter(&self, shader: Shader, pname: u32) -> bool {
        self.objects
            .borrow()
            .shaders
            .get(&shader.0)
            .and_then(|s| self.ctx.get_shader_parameter(s, pname).as_bool())
            .unwrap_or(false)
    }
    fn delete_shader(&self, shader: Shader) {
        if let Some(s) = self.objects.borrow_mut().shaders.remove(&shader.0) {
            self.ctx.delete_shader(Some(&s));
        }
    }
    fn create_program(&self) -> Option<Program> {
        create!(self, programs, self.ctx.create_program()).map(Program)
    }
    fn attach_shader(&self, program: Program, shader: Shader) {
        let o = self.objects.borrow();
        if let (Some(p), Some(s)) = (o.programs.get(&program.0), o.shaders.get(&shader.0)) {
            self.ctx.attach_shader(p, s);
        }
    }
    fn link_program(&self, program: Program) {
        if let Some(p) = self.objects.borrow().programs.get(&program.0) {
            self.ctx.link_program(p);
        }
    }
    fn get_program_parameter(&self, program: Program, pname: u32) -> bool {
        self.objects
            .borrow()
            .programs
            .get(&program.0)
            .and_then(|p| self.ctx.get_program_parameter(p, pname).as_bool())
            .unwrap_or(false)
    }
    fn delete_program(&self, program: Program) {
        if let Some(p) = self.objects.borrow_mut().programs.remove(&program.0) {
            self.ctx.delete_program(Some(&p));
        }
    }
    fn use_program(&self, program: Option<Program>) {
        let o = self.objects.borrow();
        self.ctx
            .use_program(program.and_then(|p| o.programs.get(&p.0)));
    }
    fn get_uniform_location(&self, program: Program, name: &str) -> Option<UniformLocation> {
        let location = {
            let o = self.objects.borrow();
            let p = o.programs.get(&program.0)?;
            self.ctx.get_uniform_location(p, name)
        };
        create!(self, uniforms, location).map(UniformLocation)
    }
    fn uniform1i(&self, location: Option<UniformLocation>, x: i32) {
        let o = self.objects.borrow();
        self.ctx
            .uniform1i(location.and_then(|l| o.uniforms.get(&l.0)), x);
    }
    fn uniform1f(&self, location: Option<UniformLocation>, x: f64) {
        let o = self.objects.borrow();
        self.ctx
            .uniform1f(location.and_then(|l| o.uniforms.get(&l.0)), x as f32);
    }
    fn uniform2f(&self, location: Option<UniformLocation>, x: f64, y: f64) {
        let o = self.objects.borrow();
        self.ctx.uniform2f(
            location.and_then(|l| o.uniforms.get(&l.0)),
            x as f32,
            y as f32,
        );
    }

    // ── buffers and vertex arrays ──
    fn create_buffer(&self) -> Option<Buffer> {
        create!(self, buffers, self.ctx.create_buffer()).map(Buffer)
    }
    fn delete_buffer(&self, buffer: Buffer) {
        if let Some(b) = self.objects.borrow_mut().buffers.remove(&buffer.0) {
            self.ctx.delete_buffer(Some(&b));
        }
    }
    fn bind_buffer(&self, target: u32, buffer: Option<Buffer>) {
        let o = self.objects.borrow();
        self.ctx
            .bind_buffer(target, buffer.and_then(|b| o.buffers.get(&b.0)));
    }
    fn buffer_data(&self, target: u32, data: &[f32], usage: u32) {
        // SAFETY: the view is handed to the call below and dropped with it; nothing
        // allocates in wasm memory in between, so the memory it points at cannot move.
        let view = unsafe { Float32Array::view(data) };
        self.ctx
            .buffer_data_with_array_buffer_view(target, &view, usage);
    }
    fn create_vertex_array(&self) -> Option<VertexArray> {
        create!(self, vertex_arrays, self.ctx.create_vertex_array()).map(VertexArray)
    }
    fn delete_vertex_array(&self, vao: VertexArray) {
        if let Some(v) = self.objects.borrow_mut().vertex_arrays.remove(&vao.0) {
            self.ctx.delete_vertex_array(Some(&v));
        }
    }
    fn bind_vertex_array(&self, vao: Option<VertexArray>) {
        let o = self.objects.borrow();
        self.ctx
            .bind_vertex_array(vao.and_then(|v| o.vertex_arrays.get(&v.0)));
    }
    fn enable_vertex_attrib_array(&self, index: u32) {
        self.ctx.enable_vertex_attrib_array(index);
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
        self.ctx
            .vertex_attrib_pointer_with_i32(index, size, kind, normalized, stride, offset);
    }
    fn draw_arrays(&self, mode: u32, first: i32, count: i32) {
        self.ctx.draw_arrays(mode, first, count);
    }

    // ── textures and framebuffers ──
    fn create_texture(&self) -> Option<Texture> {
        create!(self, textures, self.ctx.create_texture()).map(Texture)
    }
    fn delete_texture(&self, texture: Texture) {
        if let Some(t) = self.objects.borrow_mut().textures.remove(&texture.0) {
            self.ctx.delete_texture(Some(&t));
        }
    }
    fn active_texture(&self, texture: u32) {
        self.ctx.active_texture(texture);
    }
    fn bind_texture(&self, target: u32, texture: Option<Texture>) {
        let o = self.objects.borrow();
        self.ctx
            .bind_texture(target, texture.and_then(|t| o.textures.get(&t.0)));
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
        let _ = self
            .ctx
            .tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
                target,
                level,
                internal_format as i32,
                width,
                height,
                border,
                format,
                kind,
                None,
            );
    }
    fn tex_image_2d_canvas(
        &self,
        target: u32,
        level: i32,
        internal_format: u32,
        format: u32,
        kind: u32,
        source: &CanvasImage,
    ) {
        if let Some(value) = image_value(source) {
            vela_tex_image_source(
                &self.ctx,
                target,
                level,
                internal_format,
                format,
                kind,
                &value,
            );
        }
    }
    fn tex_parameteri(&self, target: u32, pname: u32, param: i32) {
        self.ctx.tex_parameteri(target, pname, param);
    }
    fn pixel_storei(&self, pname: u32, param: bool) {
        self.ctx.pixel_storei(pname, i32::from(param));
    }
    fn create_framebuffer(&self) -> Option<Framebuffer> {
        create!(self, framebuffers, self.ctx.create_framebuffer()).map(Framebuffer)
    }
    fn delete_framebuffer(&self, framebuffer: Framebuffer) {
        if let Some(f) = self
            .objects
            .borrow_mut()
            .framebuffers
            .remove(&framebuffer.0)
        {
            self.ctx.delete_framebuffer(Some(&f));
        }
    }
    fn bind_framebuffer(&self, target: u32, framebuffer: Option<Framebuffer>) {
        let o = self.objects.borrow();
        self.ctx
            .bind_framebuffer(target, framebuffer.and_then(|f| o.framebuffers.get(&f.0)));
    }
    fn framebuffer_texture_2d(
        &self,
        target: u32,
        attachment: u32,
        tex_target: u32,
        texture: Option<Texture>,
        level: i32,
    ) {
        let o = self.objects.borrow();
        self.ctx.framebuffer_texture_2d(
            target,
            attachment,
            tex_target,
            texture.and_then(|t| o.textures.get(&t.0)),
            level,
        );
    }

    // ── context ──
    fn is_context_lost(&self) -> bool {
        self.ctx.is_context_lost()
    }
    fn lose_context(&self) {
        if let Ok(Some(extension)) = self.ctx.get_extension("WEBGL_lose_context") {
            let extension: WebglLoseContext = extension.unchecked_into();
            extension.lose_context();
        }
    }
}

/// The data canvas as the host owns it while it paints with WebGL2.
pub struct BrowserGlCanvas {
    canvas: HtmlCanvasElement,
    gl: Rc<BrowserGl>,
    image: CanvasImage,
    /// `webglcontextlost` / `webglcontextrestored`, removed with the canvas.
    _listeners: Vec<Listener>,
}

impl BrowserGlCanvas {
    pub(crate) fn new(
        canvas: HtmlCanvasElement,
        ctx: WebGl2RenderingContext,
        listeners: Vec<Listener>,
    ) -> Rc<Self> {
        let image: CanvasImage = Rc::new(JsValue::from(canvas.clone()));
        Rc::new(Self {
            canvas,
            gl: Rc::new(BrowserGl::new(ctx)),
            image,
            _listeners: listeners,
        })
    }

    /// The `<canvas>` element.
    pub fn element(&self) -> &HtmlCanvasElement {
        &self.canvas
    }

    /// The context wrapper (for tests and diagnostics).
    pub fn browser_gl(&self) -> &Rc<BrowserGl> {
        &self.gl
    }
}

impl HostGlCanvas for BrowserGlCanvas {
    fn gl(&self) -> Rc<dyn Gl> {
        self.gl.clone()
    }

    fn image(&self) -> CanvasImage {
        self.image.clone()
    }

    fn set_size(&self, backing_width: f64, backing_height: f64, css_width: f64, css_height: f64) {
        // Assigning width / height reallocates the drawing buffer; the backend sets its
        // viewport from the backing size it is told next.
        self.canvas.set_width(backing_width as u32);
        self.canvas.set_height(backing_height as u32);
        set_styles(
            &self.canvas,
            &[
                ("width", &format!("{css_width}px")),
                ("height", &format!("{css_height}px")),
            ],
        );
    }
}

// ── choosing the backend from JavaScript ──

thread_local! {
    /// The `backend` mode and `glow` amount the next chart built on this thread starts with.
    static CONSTRUCTION: Cell<(NativeBackend, f64)> = const { Cell::new((NativeBackend::Auto, 0.0)) };
}

/// The backend mode and glow amount a renderer built now starts with: `auto` and 0, unless a
/// `withOptions` constructor is on the stack.
pub(crate) fn construction_options() -> (NativeBackend, f64) {
    CONSTRUCTION.with(Cell::get)
}

/// Run `build` (a chart constructor) with `options` as the starting backend mode and glow.
pub(crate) fn with_construction_options<R>(
    options: (NativeBackend, f64),
    build: impl FnOnce() -> R,
) -> R {
    let before = CONSTRUCTION.with(|c| c.replace(options));
    let built = build();
    CONSTRUCTION.with(|c| c.set(before));
    built
}

/// `"auto"`, `"webgl2"` or `"canvas2d"`; anything else is an error, as an unknown theme is.
pub(crate) fn parse_backend(name: &str) -> Result<NativeBackend, JsValue> {
    match name {
        "auto" => Ok(NativeBackend::Auto),
        "webgl2" => Ok(NativeBackend::Webgl2),
        "canvas2d" => Ok(NativeBackend::Canvas2d),
        other => Err(JsError::new(&format!(
            "vela: unknown backend \"{other}\" (use \"auto\", \"webgl2\" or \"canvas2d\")"
        ))
        .into()),
    }
}

/// The `{ backend, glow }` object of a `withOptions` constructor (each key optional; an
/// absent or `undefined` object means the defaults, `auto` and no glow).
pub(crate) fn render_options_of(options: &JsValue) -> Result<(NativeBackend, f64), JsValue> {
    let mut out = (NativeBackend::Auto, 0.0);
    if options.is_undefined() || options.is_null() {
        return Ok(out);
    }
    let backend =
        Reflect::get(options, &JsValue::from_str("backend")).unwrap_or(JsValue::UNDEFINED);
    if !backend.is_undefined() && !backend.is_null() {
        let name = backend
            .as_string()
            .ok_or_else(|| JsError::new("vela: options.backend must be a string"))?;
        out.0 = parse_backend(&name)?;
    }
    let glow = Reflect::get(options, &JsValue::from_str("glow")).unwrap_or(JsValue::UNDEFINED);
    if !glow.is_undefined() && !glow.is_null() {
        out.1 = glow
            .as_f64()
            .filter(|g| g.is_finite() && *g >= 0.0)
            .ok_or_else(|| JsError::new("vela: options.glow must be a non-negative number"))?;
    }
    Ok(out)
}

/// The name `backend()` reports for the geometry backend in force.
pub(crate) fn backend_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::WebGl2 => "webgl2",
        BackendKind::Canvas2d => "canvas2d",
    }
}
