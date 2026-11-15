// Parity check for the WebGL2 geometry backend of the native renderer: replays every
// scenario of a `dump_gl` JSON on the ORIGINAL TypeScript `WebGL2Backend`, on a recording
// JavaScript GL stub (recording_gl.mjs), and compares the GL call streams with what the Rust
// port produced on its `RecordingGl`.
//
// Each scenario carries a plain JSON spec (bars, viewport, pane layout, scene flags, style
// patch, indicator models, drawing slices, alphas, glow). Both sides build their own scene
// from that spec through their public calls (the scene builders below are the ones of
// compare_series.mjs, copied). Each frame is painted three times on ONE backend (the second
// pass exercises the texture / framebuffer reuse, the third one, without the drawing slices,
// their cleanup), then the backend is destroyed; both sides do exactly that.
//
// Nothing is normalized except what JSON cannot carry: a recorded NaN / Infinity argument is
// null on both sides, a negative zero is read as zero and counted (see `normalize`; the summary
// line reports the counts of both sides), and a bar the spec lists with null prices is a bar with
// NaN prices. Numbers are compared EXACTLY
// (tolerance 0): the vertex data is float32 on both sides, so any difference is a real
// difference in the arithmetic.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_gl.mjs <upstream checkout> <gl.json>
//
// Uploads of more than 64 floats are compared as digests (see recording_gl.mjs); set GL_FULL=1,
// here and for `dump_gl`, to compare every float. Exits 1 when anything differs.
// GL_SHOW=<frame> prints both streams of that frame.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { RecordingGL } from './recording_gl.mjs';
import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_gl.mjs <upstream checkout> <gl.json>');
    process.exit(2);
}

useUpstream(upstreamArg);
const root = path.resolve(upstreamArg);
const load = (rel) => import(pathToFileURL(path.join(root, rel)).href);
const native = (rel) => load(`src/renderers/native/${rel}`);

const { SceneGraph } = await native('core/SceneGraph.ts');
const { CoordinateSystem } = await native('core/CoordinateSystem.ts');
const { WebGL2Backend } = await native('backend/WebGL2Backend.ts');
const { builtinFormatters } = await load('src/core/format/formatters.ts');

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));

// ── the color probe ──
// The original turns a CSS color into RGBA floats by painting it on a 1x1 canvas it makes with
// `document.createElement('canvas')` and reading the pixel back (backend/gl/color.ts, used by
// the price tag's and the crosshair chips' text-contrast pick). Node has no DOM, so a minimal
// `document` hands out that probe. Its pixel is what a browser reads back: 8-bit alpha, channels
// stored premultiplied and un-premultiplied on the way out (the Rust port documents the same
// model in `backend/gl/color.rs`). Only the forms the scenarios use are parsed (hex, rgb/rgba);
// anything else leaves the previous fillStyle, as a canvas does with an invalid string.
function parseCssColor(css) {
    const s = String(css).trim().toLowerCase();
    let m = /^#([0-9a-f]+)$/.exec(s);
    if (m) {
        const d = m[1];
        const h = (i, len) => Number.parseInt(len === 1 ? d[i] + d[i] : d.slice(i, i + 2), 16);
        if (d.length === 3 || d.length === 4) return [h(0, 1), h(1, 1), h(2, 1), d.length === 4 ? h(3, 1) / 255 : 1];
        if (d.length === 6 || d.length === 8) return [h(0, 2), h(2, 2), h(4, 2), d.length === 8 ? h(6, 2) / 255 : 1];
        return null;
    }
    m = /^rgba?\(([^)]*)\)$/.exec(s);
    if (m) {
        const p = m[1].split(/[\s,/]+/).filter(Boolean).map(Number);
        if (p.length < 3 || p.length > 4 || p.some(Number.isNaN)) return null;
        return [p[0], p[1], p[2], p.length === 4 ? p[3] : 1];
    }
    return null;
}
const roundHalfUp = (x) => Math.floor(x + 0.5);
function probePixel(css) {
    const c = parseCssColor(css);
    if (!c) return null;
    const a8 = roundHalfUp(Math.min(1, Math.max(0, c[3])) * 255);
    if (a8 === 0) return [0, 0, 0, 0];
    const ch = c.slice(0, 3).map((v) => roundHalfUp(Math.min(255, Math.max(0, v))));
    const un = (v) => (a8 === 255 ? v : Math.min(255, roundHalfUp((roundHalfUp((v * a8) / 255) * 255) / a8)));
    return [...ch.map(un), a8];
}
function makeProbeCanvas() {
    let fill = '#000';
    let pixel = [0, 0, 0, 0];
    const ctx = {
        clearRect() {
            pixel = [0, 0, 0, 0];
        },
        set fillStyle(v) {
            if (parseCssColor(v)) fill = v;
        },
        get fillStyle() {
            return fill;
        },
        fillRect() {
            pixel = probePixel(fill);
        },
        getImageData: () => ({ data: pixel }),
    };
    return { width: 1, height: 1, getContext: () => ctx };
}
globalThis.document = { createElement: (tag) => (tag === 'canvas' ? makeProbeCanvas() : {}) };


// ── scene building (the same spec the Rust side decodes) ──

const nanify = (v) => (v === null || v === undefined ? Number.NaN : v);

function barsOf(dataset) {
    return dump.datasets[dataset].bars.map(([time, open, high, low, close, volume]) => {
        const b = { time, open: nanify(open), high: nanify(high), low: nanify(low), close: nanify(close) };
        if (volume !== null && volume !== undefined) b.volume = volume;
        return b;
    });
}

function seriesOf(s, paneId) {
    const out = { ...s, title: s.id, paneId };
    if (s.kind === 'candle' || s.kind === 'bar') {
        out.bars = s.bars.map(([time, open, high, low, close]) => ({ time, open: nanify(open), high: nanify(high), low: nanify(low), close: nanify(close) }));
    }
    if (s.kind === 'markers') out.markers = [];
    return out;
}

function modelOf(m) {
    return {
        id: m.id,
        title: m.id,
        overlay: true,
        paneHint: 'price',
        paneId: m.paneId,
        ownScale: m.ownScale === true,
        series: (m.series ?? []).map((s) => seriesOf(s, m.paneId)),
        fills: (m.fills ?? []).map((f) => ({ ...f, paneId: m.paneId })),
        backgrounds: (m.backgrounds ?? []).map((b) => ({ ...b, paneId: m.paneId })),
        priceLines: (m.priceLines ?? []).map((p) => ({ ...p, paneId: m.paneId })),
        lines: [],
        boxes: [],
        labels: [],
        polylines: [],
        linefills: [],
        tables: [],
        barColors: m.barColors ?? [],
        trades: m.trades ?? [],
        inputs: [],
        inputValues: {},
    };
}

function applyStyle(style, p) {
    for (const key of ['fontSize', 'borderColor', 'separatorColor']) if (key in p) style[key] = p[key];
    for (const section of ['gridVert', 'gridHorz', 'crosshair', 'candle', 'bars', 'line', 'area', 'baseline', 'columns', 'sessions']) {
        if (p[section]) Object.assign(style[section], p[section]);
    }
}

function buildFrame(spec) {
    const scene = new SceneGraph();
    scene.bars = barsOf(spec.dataset);
    const times = scene.bars.map((b) => b.time);
    const s = spec.scene;
    for (const key of ['priceStyle', 'basePainting', 'showPriceLine', 'showPriceLabel', 'showCountdown', 'logScale', 'invertScale', 'scaleMode', 'percentBaseline', 'timezone', 'showGrid', 'showAxisLabels', 'candleZ', 'candlesHidden', 'marksHoverKey', 'marksHoverSince', 'marksActiveKey']) {
        if (s[key] !== undefined && s[key] !== null) scene[key] = s[key];
    }
    if (s.baselineValue !== undefined) scene.baselineValue = s.baselineValue;
    if (s.priceMintick !== undefined && s.priceMintick !== null) scene.priceMintick = s.priceMintick;
    if (s.style) applyStyle(scene.style, s.style);
    if (s.candleOverride) scene.candleOverride = { ...s.candleOverride };
    if (s.crosshair) scene.crosshair = { ...s.crosshair };
    if (s.crosshairOverride) scene.crosshairOverride = JSON.parse(JSON.stringify(s.crosshairOverride));
    if (s.highlights) scene.highlights = s.highlights.map((h) => ({ ...h }));
    if (s.sessionZones) scene.sessionZones = JSON.parse(JSON.stringify(s.sessionZones));
    if (s.tradeMarkers) scene.tradeMarkers = { ...scene.tradeMarkers, ...s.tradeMarkers };
    if (s.marks) scene.marks = { visible: s.marks.visible, groups: { ...s.marks.groups } };
    if (s.timelineMarks) scene.timelineMarks = s.timelineMarks;
    if (s.markGroups) scene.markGroups = s.markGroups;
    if (s.marksExpandedStack !== undefined && s.marksExpandedStack !== null) scene.marksExpandedStack = s.marksExpandedStack;
    if (s.format) scene.format = builtinFormatters({ locale: s.format.locale, numberingSystem: s.format.numberingSystem, hourCycle: s.format.hourCycle, english: s.format.english });
    if (s.marksFlash) scene.marksFlash = { ...s.marksFlash };

    for (const p of spec.panes) {
        const pane = scene.ensurePane(p.id, p.kind, p.order, p.heightWeight);
        pane.bounds = { top: p.top, height: p.height };
        pane.scale = { ...p.scale };
        pane.scaleTarget = { ...p.scale };
        pane.initialized = true;
        pane.collapsed = p.collapsed === true;
        if (p.axisFormat) pane.axisFormat = p.axisFormat;
        if (p.axisBands) pane.axisBands = p.axisBands.map((b) => ({ ...b }));
        if (p.scaleMode) pane.scaleMode = p.scaleMode;
        if (p.logScale !== undefined) pane.logScale = p.logScale;
        if (p.invert !== undefined) pane.invert = p.invert;
        pane.percentBaseline = p.percentBaseline ?? 0;
    }
    for (const m of spec.indicators) {
        scene.indicators.set(m.id, modelOf(m));
        if (m.z !== undefined && m.z !== null) scene.setIndicatorZ(m.id, m.z);
        else scene.assignIndicatorZ(m.id);
        if (m.anchorOffset !== undefined) scene.setAnchorOffset(m.id, m.anchorOffset);
        if (m.ownScaleWindow) {
            const slot = scene.ensureIndicatorScale(m.id);
            slot.scale = { ...m.ownScaleWindow };
            slot.scaleTarget = { ...m.ownScaleWindow };
            slot.initialized = true;
        }
    }
    scene.drawingSlices = new Map(Object.entries(spec.slices).map(([paneId, list]) => [paneId, list.map((z) => ({ beforeZ: z ?? Number.POSITIVE_INFINITY, canvas: {} }))]));

    const coords = new CoordinateSystem();
    coords.setSize(spec.width, spec.height, spec.dpr);
    coords.setBars(times);
    coords.setViewport({ barSpacing: spec.viewport.barSpacing, rightOffset: spec.viewport.rightOffset });
    if (spec.pitchScale !== undefined) coords.setPitchScale(spec.pitchScale);
    return { scene, coords, theme: dump.themes[spec.theme] };
}


// ── replay ──

/** The fake `HTMLCanvasElement` the backend mounts on: its WebGL context and backing store. */
function fakeCanvas(gl, spec) {
    return {
        width: spec.fullWidth * spec.dpr,
        height: spec.fullHeight * spec.dpr,
        getContext: (kind) => (kind === 'webgl2' ? gl : null),
        addEventListener() {},
        removeEventListener() {},
    };
}

const differ = new Differ(0);
const failures = [];
let calls = 0;
let frames = 0;
let withGlow = 0;
let withSlices = 0;
let draws = 0;
const skipped = [];
const negativeZeros = { original: 0, rust: 0 };
const byName = {};

function replay(spec) {
    const { scene, coords, theme } = buildFrame(spec);
    const gl = new RecordingGL(process.env.GL_FULL ? {} : { digestAbove: 64 });
    const backend = new WebGL2Backend();
    backend.glow = spec.glow ?? 0;
    backend.mount(fakeCanvas(gl, spec));
    backend.modelAlpha = spec.alphas.model;
    backend.candleBodyAlpha = spec.alphas.body;
    backend.candleStructureAlpha = spec.alphas.structure;
    backend.candleBodyScale = spec.alphas.scale;
    backend.render(scene, coords, theme);
    backend.render(scene, coords, theme);
    scene.drawingSlices = new Map();
    backend.render(scene, coords, theme);
    backend.destroy();
    return normalize(gl.ops, 'original');
}

/** What JSON cannot carry, made explicit on both sides: a non-finite number is null (both
 *  writers print NaN as null). A negative zero is counted and read as zero: the original's
 *  `Math.round` can return -0 (so a vertex coordinate is -0) where the port's `js_round`
 *  returns +0, which no GPU or rasterizer can tell apart. The counts are reported in the
 *  summary line (`negativeZeros`), so the difference is verified rather than ignored; the digest
 *  of an upload carries its own count (`nz`) and reads -0 as 0. */
function normalize(v, side) {
    if (typeof v === 'number') {
        if (!Number.isFinite(v)) return null;
        if (Object.is(v, -0)) {
            negativeZeros[side] += 1;
            return 0;
        }
        return v;
    }
    if (Array.isArray(v)) return v.map((x) => normalize(x, side));
    if (v && typeof v === 'object') {
        if ('nz' in v && 'f32' in v) {
            negativeZeros[side] += v.nz;
            const { nz, ...rest } = v;
            return rest;
        }
        return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, normalize(x, side)]));
    }
    return v;
}

dump.frames.forEach((sc, n) => {
    if (sc === null) return; // GL_ONLY=<frame> in dump_gl leaves the others out
    if (sc.skipped) {
        // dump_gl.rs: a dashed polyline far outside its pane is dashed before any clipping, on
        // both sides (gigabytes of vertices); the frame cannot be replayed and is not compared.
        skipped.push(`frame ${n}: ${sc.skipped}`);
        return;
    }
    frames += 1;
    const spec = sc.spec;
    const what = `frame ${n} (${dump.datasets[spec.dataset].name}, ${spec.scene.priceStyle}, glow ${spec.glow})`;
    let original;
    try {
        original = replay(spec);
    } catch (err) {
        failures.push(`${what}: the original threw ${err?.stack ?? err}`);
        return;
    }
    if (process.env.GL_SHOW === String(n)) console.log(JSON.stringify({ original, rust: sc.ops }));
    calls += original.length;
    if (spec.glow > 0) withGlow += 1;
    if (Object.values(spec.slices ?? {}).some((l) => l.length > 0)) withSlices += 1;
    draws += original.filter((o) => o[0] === 'drawArrays').length;
    for (const o of original) byName[o[0]] = (byName[o[0]] ?? 0) + 1;
    const rust = normalize(sc.ops, 'rust');
    const d = differ.diff(original, rust, '');
    if (!d) return;
    const at = Number(/^\[(\d+)\]/.exec(d)?.[1] ?? 0);
    const near = (list) => JSON.stringify(list.slice(Math.max(0, at - 2), at + 2)).slice(0, 600);
    let detail = `\n      original ${near(original)}\n      rust     ${near(rust)}`;
    if (original.length !== sc.ops.length) detail += `\n      stream lengths: original ${original.length} vs rust ${rust.length}`;
    failures.push(`${what} ${d.slice(0, 300)}${detail}`);
});

const failed = new Set(failures.map((f) => f.split(' ').slice(0, 2).join(' '))).size;
console.log(
    `${frames} frames, ${calls} GL calls compared (${draws} draw calls, ${withGlow} frames with glow, ${withSlices} with drawing slices, ` +
        `${differ.compared} numbers, tolerance 0; ${skipped.length} frames not replayable; negative zeros: original ${negativeZeros.original}, rust ${negativeZeros.rust}): ${failures.length === 0 ? 'all identical' : `${failed} frames DIFFER`}`,
);
if (process.env.GL_SKIPPED) for (const s of skipped) console.log(`  skipped ${s}`);
if (process.env.GL_CALLS) console.log(JSON.stringify(byName));
if (failures.length > 0) {
    for (const f of failures.slice(0, 20)) console.log(`  ${f}`);
    if (failures.length > 20) console.log(`  ... and ${failures.length - 20} more`);
    process.exit(1);
}
