// Shared fake DOM for the vela-web smoke checks (smoke_chart.mjs, smoke_legend.mjs, ...).
//
// The fake document, elements and canvases implement only what the wiring touches; every
// canvas hands out a RecordingContext (see recording_canvas.mjs), and requestAnimationFrame,
// ResizeObserver and matchMedia are driven by hand. It does NOT run a browser: real layout,
// real pointer capture, real painting and real devicePixelContentBox reports are not
// exercised. Importing this module installs the globals (document, performance, rAF,
// ResizeObserver, matchMedia, devicePixelRatio).

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { RecordingContext } from './recording_canvas.mjs';
import { RecordingGL } from './recording_gl.mjs';

export const state = { failures: 0 };

// Additive switches of the fake DOM, off unless a smoke test turns them on.
export const options = { captureAncestors: false, webgl: false };

// The fake `webgl2` context (off unless `options.webgl`: every canvas then answers
// `getContext('webgl2')` with null and stays 2D). A canvas takes one kind of context for
// good, as a real one does. `glControl` drives the failure paths: `available` false (no WebGL2
// in this browser), `linkOk` false (every program fails to link). `contexts` lists every
// context handed out, oldest first.
export const glControl = { available: true, linkOk: true, contexts: [] };

export class FakeGL extends RecordingGL {
    constructor(canvas) {
        super({ digestAbove: 256 });
        this.canvas = canvas;
        this.lost = false;
        glControl.contexts.push(this);
    }
    getProgramParameter(p, name) {
        super.getProgramParameter(p, name);
        return glControl.linkOk;
    }
    isContextLost() {
        super.isContextLost();
        return this.lost;
    }
    getExtension(name) {
        const ext = super.getExtension(name);
        const record = ext.loseContext;
        ext.loseContext = () => {
            record();
            this.lost = true;
        };
        return ext;
    }
    /** The recorded calls named `name`. */
    calls(name) {
        return this.ops.filter((op) => op[0] === name);
    }
    clearOps() {
        this.ops.length = 0;
    }
}

// A Map-backed `localStorage` (the demos' and the workspace's persistence); `failing` makes
// every call throw, as a blocked store does.
export const storage = new Map();
export const storageControl = { failing: false };
globalThis.localStorage = {
    getItem(key) {
        if (storageControl.failing) throw new Error('blocked');
        return storage.has(key) ? storage.get(key) : null;
    },
    setItem(key, value) {
        if (storageControl.failing) throw new Error('blocked');
        storage.set(key, String(value));
    },
    removeItem(key) {
        if (storageControl.failing) throw new Error('blocked');
        storage.delete(key);
    },
};
export function check(name, ok, detail = '') {
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` (${detail})` : ''}`);
    if (!ok) state.failures += 1;
}

// ── fake DOM ──

export class FakeStyle {
    constructor() {
        this.props = new Map();
    }
    setProperty(name, value) {
        if (value === '' || value == null) this.props.delete(name);
        else this.props.set(name, String(value));
    }
    removeProperty(name) {
        this.props.delete(name);
    }
    getPropertyValue(name) {
        return this.props.get(name) ?? '';
    }
}

export class FakeNode {
    constructor(tag, doc) {
        this.tagName = tag.toUpperCase();
        this.ownerDocument = doc;
        this.children = [];
        this.parentNode = null;
        this.style = new FakeStyle();
        this.attrs = new Map();
        this.listeners = [];
        this.id = '';
        this.dir = '';
        this.tabIndex = -1;
        this.rect = null;
        // text fields (the inline text editor's textarea, the color input)
        this.value = '';
        this.selectionStart = 0;
        this.selectionEnd = 0;
    }
    // The `class` attribute and `className` are one thing, as in the DOM (additive: code that
    // reads either side sees the other).
    get className() {
        return this.attrs.get('class') ?? '';
    }
    set className(v) {
        this.attrs.set('class', String(v));
    }
    setSelectionRange(start, end) {
        this.selectionStart = start;
        this.selectionEnd = end;
    }
    setRangeText(text, start, end, mode) {
        this.value = this.value.slice(0, start) + text + this.value.slice(end);
        if (mode === 'end') this.selectionStart = this.selectionEnd = start + text.length;
    }
    get firstChild() {
        return this.children[0] ?? null;
    }
    // Additive (smoke_legend): the DOM's text / markup setters replace the children, the
    // element-only traversals.
    get textContent() {
        return this._text ?? this.children.map((c) => c.textContent).join('');
    }
    set textContent(value) {
        this._text = String(value ?? '');
        this.children = [];
    }
    get innerHTML() {
        return this._html ?? '';
    }
    set innerHTML(value) {
        this._html = String(value ?? '');
        this.children = [];
    }
    get parentElement() {
        return this.parentNode && this.parentNode.tagName && !this.parentNode.tagName.startsWith('#') ? this.parentNode : null;
    }
    get firstElementChild() {
        return this.children[0] ?? null;
    }
    get nextElementSibling() {
        const sibs = this.parentNode?.children;
        return sibs ? (sibs[sibs.indexOf(this) + 1] ?? null) : null;
    }
    appendChild(node) {
        this._text = undefined;
        node.remove();
        node.parentNode = this;
        this.children.push(node);
        return node;
    }
    insertBefore(node, ref) {
        if (ref == null) return this.appendChild(node);
        this._text = undefined;
        node.remove();
        const at = this.children.indexOf(ref);
        node.parentNode = this;
        this.children.splice(at < 0 ? this.children.length : at, 0, node);
        return node;
    }
    prepend(node) {
        return this.insertBefore(node, this.children[0] ?? null);
    }
    remove() {
        if (!this.parentNode) return;
        const siblings = this.parentNode.children;
        siblings.splice(siblings.indexOf(this), 1);
        this.parentNode = null;
    }
    contains(node) {
        for (let n = node; n; n = n.parentNode) if (n === this) return true;
        return false;
    }
    setAttribute(name, value) {
        this.attrs.set(name, String(value));
    }
    getAttribute(name) {
        return this.attrs.get(name) ?? null;
    }
    removeAttribute(name) {
        this.attrs.delete(name);
    }
    hasAttribute(name) {
        return this.attrs.has(name);
    }
    addEventListener(type, handler, options) {
        const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
        this.listeners.push({ type, handler, capture });
    }
    removeEventListener(type, handler, capture) {
        const cap = typeof capture === 'boolean' ? capture : Boolean(capture?.capture);
        const at = this.listeners.findIndex((l) => l.type === type && l.handler === handler && l.capture === cap);
        if (at >= 0) this.listeners.splice(at, 1);
    }
    // Fires the target's listeners, then (unless the event does not bubble or a handler
    // stops it) walks up: parents, the document, the window. Capture-phase listeners on
    // ancestors are not run (nothing here depends on them).
    dispatch(type, event) {
        const e = {
            target: this,
            bubbles: true,
            stopped: false,
            preventDefault() { this.defaultPrevented = true; },
            stopPropagation() { this.stopped = true; },
            stopImmediatePropagation() { this.stopped = true; },
            ...event,
        };
        const doc = this.ownerDocument;
        const next = (n) => {
            if (n.parentNode) return n.parentNode;
            if (n === doc) return doc.defaultView;
            if (n === doc?.defaultView) return null;
            return doc ?? null;
        };
        // Opt-in (`options.captureAncestors`): the capture phase. Capture listeners of the
        // ancestors run first, outermost first, before the target's own listeners.
        if (options.captureAncestors) {
            const path = [];
            for (let n = next(this); n; n = next(n)) path.unshift(n);
            for (const n of path) {
                e.currentTarget = n;
                for (const l of [...n.listeners]) {
                    if (l.type === type && l.capture && !e.stopped) l.handler(e);
                }
            }
            e.stopped = false; // stopPropagation in the capture phase is not modeled further
        }
        for (let n = this; n; n = e.bubbles && !e.stopped ? next(n) : null) {
            e.currentTarget = n;
            for (const l of [...n.listeners]) {
                if (l.type === type && (n === this || !l.capture)) l.handler(e);
            }
        }
        return e;
    }
    getBoundingClientRect() {
        const r = this.rect ?? this.parentNode?.getBoundingClientRect?.() ?? { left: 0, top: 0, width: 0, height: 0 };
        const left = Number.parseFloat(this.style.getPropertyValue('left')) || 0;
        return { left: r.left + left, top: r.top, width: r.width - left, height: r.height, right: r.left + r.width, bottom: r.top + r.height };
    }
    get clientWidth() {
        return Math.round(this.getBoundingClientRect().width);
    }
    get clientHeight() {
        return Math.round(this.getBoundingClientRect().height);
    }
    focus() {
        this.ownerDocument.activeElement = this;
    }
    setPointerCapture() {}
    releasePointerCapture() {}
    all(predicate, out = []) {
        for (const c of this.children) {
            if (predicate(c)) out.push(c);
            c.all(predicate, out);
        }
        return out;
    }
}

export class FakeCanvas extends FakeNode {
    constructor(doc) {
        super('canvas', doc);
        this.ctx = new RecordingContext(this);
        this._w = 300;
        this._h = 150;
        this.resizes = 0;
    }
    get width() {
        return this._w;
    }
    set width(v) {
        this._w = v;
        this.resizes += 1;
        this.ctx.ops = []; // assigning width clears the bitmap
    }
    get height() {
        return this._h;
    }
    set height(v) {
        this._h = v;
        this.ctx.ops = [];
    }
    getContext(kind, attrs) {
        if (kind === 'webgl2') {
            if (!options.webgl || !glControl.available || this.contextKind === '2d') return null;
            this.contextKind = 'webgl2';
            this.glAttrs = attrs;
            this.gl ??= new FakeGL(this);
            return this.gl;
        }
        if (kind !== '2d' || this.contextKind === 'webgl2') return null;
        this.contextKind = '2d';
        return this.ctx;
    }
}

const DRAWING = new Set(['fill', 'stroke', 'fillRect', 'strokeRect', 'fillText', 'strokeText', 'drawImage', 'clearRect']);
export const painted = (canvas) => canvas.ctx.ops.some((op) => DRAWING.has(op[0]));

export class FakeDocument extends FakeNode {
    constructor() {
        super('#document', null);
        this.ownerDocument = this;
        this.head = new FakeNode('head', this);
        this.documentElement = new FakeNode('html', this);
        this.defaultView = new FakeNode('window', this);
        this.activeElement = null;
    }
    createElement(tag) {
        return tag === 'canvas' ? new FakeCanvas(this) : new FakeNode(tag, this);
    }
    getElementById(id) {
        return this.head.all((n) => n.id === id)[0] ?? null;
    }
}

export const doc = new FakeDocument();
globalThis.document = doc;

// Hand-driven frame loop and clock. `clock` is a live binding: read it, advance it with tick().
export let clock = 1000;
export const tick = (ms) => {
    clock += ms;
};
Object.defineProperty(globalThis, 'performance', { value: { now: () => clock }, configurable: true, writable: true });
export const frames = new Map();
let nextFrame = 1;
globalThis.requestAnimationFrame = (cb) => {
    frames.set(nextFrame, cb);
    return nextFrame++;
};
globalThis.cancelAnimationFrame = (id) => frames.delete(id);
export function pump(maxFrames = 400) {
    let ran = 0;
    while (frames.size > 0 && ran < maxFrames) {
        clock += 16;
        const due = [...frames.entries()];
        frames.clear();
        for (const [, cb] of due) cb(clock);
        ran += 1;
    }
    return ran;
}

// ResizeObserver and matchMedia, fired by hand.
export const observers = [];
globalThis.ResizeObserver = class {
    constructor(cb) {
        this.cb = cb;
        this.targets = [];
        this.connected = true;
        observers.push(this);
    }
    observe(el, options) {
        this.targets.push({ el, options });
    }
    disconnect() {
        this.connected = false;
    }
};
globalThis.devicePixelRatio = 1;
export const dprWatchers = [];
globalThis.matchMedia = () => {
    const listeners = [];
    const query = {
        addEventListener: (_type, fn) => listeners.push(fn),
        removeEventListener: (_type, fn) => listeners.splice(listeners.indexOf(fn), 1),
        listeners,
    };
    dprWatchers.push(query);
    return query;
};

// ── the package ──

// Loads the wasm package from the directory named by argv[2] (usage: <script> <pkg dir>).
export async function loadPackage(scriptName) {
    const pkgArg = process.argv[2];
    if (!pkgArg) {
        console.error(`usage: ${scriptName} <dir with vela_web.js>`);
        process.exit(2);
    }
    const pkg = path.resolve(pkgArg);
    const wasm = await import(pathToFileURL(path.join(pkg, 'vela_web.js')).href);
    await wasm.default({ module_or_path: readFileSync(path.join(pkg, 'vela_web_bg.wasm')) });
    return wasm;
}

export function makeBars(n, start = Date.UTC(2025, 0, 1), step = 3_600_000) {
    let seed = 12345;
    const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
    const bars = [];
    let price = 100;
    for (let i = 0; i < n; i += 1) {
        const open = price;
        const close = open + (rnd() - 0.5) * 2;
        const high = Math.max(open, close) + rnd();
        const low = Math.min(open, close) - rnd();
        bars.push({ time: start + i * step, open, high, low, close, volume: 1000 + Math.floor(rnd() * 500) });
        price = close;
    }
    return bars;
}

export function container(width, height) {
    const el = doc.createElement('div');
    el.rect = { left: 0, top: 0, width, height };
    doc.documentElement.appendChild(el);
    return el;
}

export const pileOf = (host) => {
    const wrapper = host.children[0];
    const plot = wrapper.children[0];
    return { wrapper, plot, canvases: plot.children.filter((c) => c instanceof FakeCanvas) };
};
