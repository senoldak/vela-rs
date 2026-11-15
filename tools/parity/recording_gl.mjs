// A WebGL2 context stand-in that records every call the way `RecordingGl`
// (crates/vela-core/src/render/gl.rs) does, so the call stream of the ORIGINAL
// `WebGL2Backend` can be compared with the Rust port's.
//
// Format (see the module documentation of gl.rs): each call is `[name, arg...]`; numbers and
// booleans as they are; a GL object is the string `"<kind>#<n>"` (n counts the objects of that
// kind created so far, from 1; every `getUniformLocation` call makes a new `uniform`); null
// stays null; a Float32Array becomes the array of its numbers in full (the doubles are the
// exact widened floats), or, for an upload longer than `digestAbove` floats, the digest
// object {f32: length, nz, h1, h2} (h1: 32-bit FNV-1a, offset 2166136261, prime 16777619,
// over the little-endian bytes of each float in order; h2: the same with offset 0x9747b28c
// over the floats in REVERSE order; any NaN counts as the bits 0x7fc00000, a negative zero
// as zero, and nz counts the negative zeros); `texImage2D` from a canvas records the source as the string
// "canvas", and the pixel-less form records `null`; `getExtension(name)` is recorded and
// returns an object whose `loseContext()` is recorded too.
//
// The stub answers what the backend asks: shaders compile and programs link, the context is
// not lost, `createX` always returns an object. Constants carry their real GL values.

const KINDS = ['shader', 'program', 'buffer', 'vertexArray', 'texture', 'framebuffer', 'uniform'];

export const GL_CONSTANTS = {
    ARRAY_BUFFER: 0x8892,
    STATIC_DRAW: 0x88e4,
    DYNAMIC_DRAW: 0x88e8,
    TRIANGLES: 0x0004,
    TRIANGLE_STRIP: 0x0005,
    FLOAT: 0x1406,
    UNSIGNED_BYTE: 0x1401,
    VERTEX_SHADER: 0x8b31,
    FRAGMENT_SHADER: 0x8b30,
    COMPILE_STATUS: 0x8b81,
    LINK_STATUS: 0x8b82,
    BLEND: 0x0be2,
    SCISSOR_TEST: 0x0c11,
    COLOR_BUFFER_BIT: 0x4000,
    ZERO: 0,
    ONE: 1,
    SRC_ALPHA: 0x0302,
    ONE_MINUS_SRC_ALPHA: 0x0303,
    TEXTURE_2D: 0x0de1,
    TEXTURE0: 0x84c0,
    RGBA: 0x1908,
    TEXTURE_MIN_FILTER: 0x2801,
    TEXTURE_MAG_FILTER: 0x2800,
    TEXTURE_WRAP_S: 0x2802,
    TEXTURE_WRAP_T: 0x2803,
    LINEAR: 0x2601,
    CLAMP_TO_EDGE: 0x812f,
    FRAMEBUFFER: 0x8d40,
    COLOR_ATTACHMENT0: 0x8ce0,
    UNPACK_FLIP_Y_WEBGL: 0x9240,
    UNPACK_PREMULTIPLY_ALPHA_WEBGL: 0x9241,
};

/** The digest of a Float32Array upload (see the header). */
export function digestFloats(f32) {
    const words = new Uint32Array(f32.buffer, f32.byteOffset, f32.length);
    let nz = 0;
    for (let i = 0; i < f32.length; i++) if (Object.is(f32[i], -0)) nz += 1;
    const word = (i) => (Number.isNaN(f32[i]) ? 0x7fc00000 : f32[i] === 0 ? 0 : words[i]);
    const mix = (h, w) => {
        for (let k = 0; k < 4; k++) h = Math.imul(h ^ ((w >>> (8 * k)) & 255), 16777619);
        return h >>> 0;
    };
    let h1 = 2166136261;
    for (let i = 0; i < f32.length; i++) h1 = mix(h1, word(i));
    let h2 = 0x9747b28c;
    for (let i = f32.length - 1; i >= 0; i--) h2 = mix(h2, word(i));
    return { f32: f32.length, nz, h1, h2 };
}

export class RecordingGL {
    /** @param {{digestAbove?: number}} [options] record uploads of more floats than this as a digest */
    constructor(options = {}) {
        this.digestAbove = options.digestAbove ?? Number.POSITIVE_INFINITY;
        this.ops = [];
        this.counters = Object.fromEntries(KINDS.map((k) => [k, 0]));
        Object.assign(this, GL_CONSTANTS);
    }

    /** The recorded calls. */
    toJSON() {
        return this.ops;
    }

    #make(kind) {
        this.counters[kind] += 1;
        return { __gl: kind, n: this.counters[kind] };
    }

    #arg(v) {
        if (v === null || v === undefined) return null;
        if (typeof v === 'object' && v.__gl) return `${v.__gl}#${v.n}`;
        if (v instanceof Float32Array) return v.length > this.digestAbove ? digestFloats(v) : Array.from(v);
        if (typeof v === 'object') return 'canvas';
        return v;
    }

    #rec(name, ...args) {
        this.ops.push([name, ...args.map((a) => this.#arg(a))]);
    }

    viewport(...a) { this.#rec('viewport', ...a); }
    scissor(...a) { this.#rec('scissor', ...a); }
    enable(c) { this.#rec('enable', c); }
    disable(c) { this.#rec('disable', c); }
    clearColor(...a) { this.#rec('clearColor', ...a); }
    clear(m) { this.#rec('clear', m); }
    blendFunc(...a) { this.#rec('blendFunc', ...a); }
    blendFuncSeparate(...a) { this.#rec('blendFuncSeparate', ...a); }

    createShader(type) {
        this.#rec('createShader', type);
        return this.#make('shader');
    }
    shaderSource(sh, src) { this.#rec('shaderSource', sh, src); }
    compileShader(sh) { this.#rec('compileShader', sh); }
    getShaderParameter(sh, p) {
        this.#rec('getShaderParameter', sh, p);
        return true;
    }
    deleteShader(sh) { this.#rec('deleteShader', sh); }
    createProgram() {
        this.#rec('createProgram');
        return this.#make('program');
    }
    attachShader(p, s) { this.#rec('attachShader', p, s); }
    linkProgram(p) { this.#rec('linkProgram', p); }
    getProgramParameter(p, name) {
        this.#rec('getProgramParameter', p, name);
        return true;
    }
    deleteProgram(p) { this.#rec('deleteProgram', p); }
    useProgram(p) { this.#rec('useProgram', p); }
    getUniformLocation(p, name) {
        this.#rec('getUniformLocation', p, name);
        return this.#make('uniform');
    }
    uniform1i(l, x) { this.#rec('uniform1i', l, x); }
    uniform1f(l, x) { this.#rec('uniform1f', l, x); }
    uniform2f(l, x, y) { this.#rec('uniform2f', l, x, y); }

    createBuffer() {
        this.#rec('createBuffer');
        return this.#make('buffer');
    }
    deleteBuffer(b) { this.#rec('deleteBuffer', b); }
    bindBuffer(t, b) { this.#rec('bindBuffer', t, b); }
    bufferData(t, data, usage) { this.#rec('bufferData', t, data, usage); }
    createVertexArray() {
        this.#rec('createVertexArray');
        return this.#make('vertexArray');
    }
    deleteVertexArray(v) { this.#rec('deleteVertexArray', v); }
    bindVertexArray(v) { this.#rec('bindVertexArray', v); }
    enableVertexAttribArray(i) { this.#rec('enableVertexAttribArray', i); }
    vertexAttribPointer(...a) { this.#rec('vertexAttribPointer', ...a); }
    drawArrays(...a) { this.#rec('drawArrays', ...a); }

    createTexture() {
        this.#rec('createTexture');
        return this.#make('texture');
    }
    deleteTexture(t) { this.#rec('deleteTexture', t); }
    activeTexture(t) { this.#rec('activeTexture', t); }
    bindTexture(t, tex) { this.#rec('bindTexture', t, tex); }
    texImage2D(...a) { this.#rec('texImage2D', ...a); }
    texParameteri(...a) { this.#rec('texParameteri', ...a); }
    pixelStorei(...a) { this.#rec('pixelStorei', ...a); }
    createFramebuffer() {
        this.#rec('createFramebuffer');
        return this.#make('framebuffer');
    }
    deleteFramebuffer(f) { this.#rec('deleteFramebuffer', f); }
    bindFramebuffer(t, f) { this.#rec('bindFramebuffer', t, f); }
    framebufferTexture2D(...a) { this.#rec('framebufferTexture2D', ...a); }

    isContextLost() {
        this.#rec('isContextLost');
        return false;
    }
    getExtension(name) {
        this.#rec('getExtension', name);
        return { loseContext: () => this.#rec('loseContext') };
    }
}
