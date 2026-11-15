// Parity check for the canvas painters of the native renderer: replays every scenario of a
// `dump_series` JSON on the ORIGINAL TypeScript painters (Canvas2dBackend, BackdropRenderer,
// ChromeRenderer, CrosshairRenderer, the timeline-mark lane painter, the volume layer and
// the VPVR layer), painting on a recording context, and compares the canvas call streams
// with what the Rust port produced.
//
// Each scenario carries a plain JSON spec (bars, viewport, pane layout, scene flags, style
// patch, indicator models, layer inputs, clocks). Both sides build their own scene from
// that spec through their public calls: here `new SceneGraph()` + `ensurePane` + field
// assignments + `new CoordinateSystem()` + `setSize/setBars/setViewport`.
//
// Normalizations (everything else is compared as recorded):
//  - A spec `null` means "absent" in optional fields (the Rust side writes an absent
//    optional as null; the original expects it missing or null).
//  - A bar (dataset or candle series) the spec lists with `null` prices is a bar with NaN
//    prices on both sides (JSON has no NaN).
//  - The original's color probe (`document.createElement('canvas')` read back through a 1x1
//    canvas, see `makeProbeCanvas`) is a small stand-in that parses hex and rgb()/rgba() and
//    reads the pixel back the way a browser does (8-bit alpha, premultiplied storage).
//  - Three DOCUMENTED deviations of the port are verified rather than ignored (see
//    `reconcileNanRange`, `reconcileInvalidDate`, `reconcileInvalidTimeThrow`): a NaN visible
//    range in the geometry backend, an invalid time value in the time chips and axis, and
//    the RangeError the original's Intl clock throws on one. The summary counts how many
//    scenarios needed one.
//  - Anything else that differs fails the run, including behavior the Rust sources do not
//    document (the original's VPVR layer throwing on a NaN visible range; the calendar ticks
//    of a view that reaches back before the year 100).
//  - The recorders already normalize an omitted trailing `counterclockwise` argument and a
//    one-element `roundRect` radius list (see recording_canvas.mjs).
//  - The browser globals the original reads are stubbed with the scenario's own clock:
//    `Date.now()` (the bar-close countdown) and `performance.now()` (the mark lane's pulse
//    and flash). The Rust side is handed the same two numbers through `FrameClock`.
//  - The original rasterizes mark icons through the DOM (absent here, so it paints a bare
//    token) while the Rust painter asks an icon source; the lane scenarios give both sides
//    the same fake icon source that logs its requests, which are compared too.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_series.mjs <upstream checkout> <series.json>
//
// Exits 1 when anything differs. SERIES_SHOW=<frame>:<painter> prints both streams of that frame.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { RecordingContext, recordedTextMetrics } from './recording_canvas.mjs';
import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_series.mjs <upstream checkout> <series.json>');
    process.exit(2);
}

useUpstream(upstreamArg);
const root = path.resolve(upstreamArg);
const load = (rel) => import(pathToFileURL(path.join(root, rel)).href);
const native = (rel) => load(`src/renderers/native/${rel}`);

const { SceneGraph } = await native('core/SceneGraph.ts');
const { CoordinateSystem } = await native('core/CoordinateSystem.ts');
const { Canvas2dBackend } = await native('backend/Canvas2dBackend.ts');
const { BackdropRenderer } = await native('backdrop/BackdropRenderer.ts');
const { ChromeRenderer } = await native('chrome/ChromeRenderer.ts');
const { CrosshairRenderer } = await native('chrome/CrosshairRenderer.ts');
const { PriceAxisTickSource } = await native('chrome/priceAxisTicks.ts');
const { layoutMarkLane } = await native('chrome/marks/layout.ts');
const { paintMarkLane } = await native('chrome/marks/paint.ts');
const { VolumeRenderer } = await native('volume/VolumeRenderer.ts');
const { paintVolume } = await native('volume/paintVolume.ts');
const { VpvrRenderer } = await native('vpvr/VpvrRenderer.ts');
const { paintVpvr } = await native('vpvr/paintVpvr.ts');
const { builtinFormatters } = await load('src/core/format/formatters.ts');
const { volumeLayerData } = await load('src/core/native-indicators/volume/VolumeIndicator.ts');
const { vpvrLayerData } = await load('src/core/native-indicators/vpvr/VpvrIndicator.ts');

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

const differ = new Differ();
const failures = [];
let calls = 0;
let scenarios = 0;
const perPainter = {};

function check(original, rust, what, ops) {
    const d = differ.diff(original, rust, '');
    if (!d) return true;
    let detail = '';
    if (ops && Array.isArray(original) && Array.isArray(rust)) {
        const at = Number(/^\[(\d+)\]/.exec(d)?.[1] ?? 0);
        const near = (list) => JSON.stringify(list.slice(Math.max(0, at - 3), at + 2));
        detail = `\n      original ${near(original)}\n      rust     ${near(rust)}`;
        const texts = (list) => list.filter((o) => o[0] === 'fillText').map((o) => o[1]);
        const [ta, tr] = [texts(original), texts(rust)];
        const at2 = ta.findIndex((t, i) => t !== tr[i]);
        if (at2 >= 0) detail += `\n      first differing text: original ${JSON.stringify(ta[at2])} vs rust ${JSON.stringify(tr[at2])}`;
        if (original.length !== rust.length) {
            const longer = original.length > rust.length ? original : rust;
            const names = {};
            for (const o of longer.slice(Math.min(original.length, rust.length))) names[o[0]] = (names[o[0]] ?? 0) + 1;
            detail += `\n      the ${longer === original ? 'original' : 'rust'} stream continues with ${JSON.stringify(names)}`;
        }
    }
    failures.push(`${what} ${d}${detail}`);
    return false;
}

function count(name, ok, ops) {
    const p = (perPainter[name] ??= { scenarios: 0, calls: 0, failed: 0 });
    p.scenarios += 1;
    p.calls += ops;
    if (!ok) p.failed += 1;
    calls += ops;
}

const plain = (v) => JSON.parse(JSON.stringify(v ?? null));

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

/** The fake `HTMLCanvasElement` the original painters mount on: its 2d context and backing store. */
function fakeCanvas(ctx, spec) {
    return { width: spec.fullWidth * spec.dpr, height: spec.fullHeight * spec.dpr, style: {}, getContext: () => ctx };
}

const newContext = (spec) => new RecordingContext({ width: spec.fullWidth * spec.dpr, height: spec.fullHeight * spec.dpr });

// ── browser clocks ──
const realNow = Date.now;
function withClock(clock, fn) {
    Date.now = () => clock.wallMs;
    const saved = Object.getOwnPropertyDescriptor(globalThis, 'performance');
    Object.defineProperty(globalThis, 'performance', { value: { now: () => clock.nowMs }, configurable: true, writable: true });
    try {
        return fn();
    } finally {
        Date.now = realNow;
        if (saved) Object.defineProperty(globalThis, 'performance', saved);
    }
}

// ── documented deviations of the port, verified instead of ignored ──
// Both are stated in the Rust module docs. A scenario that hits one is normalized ONLY in the
// way the deviation describes (anything else in the stream still has to match), and the
// summary line counts how many scenarios needed it.
let documentedDeviations = 0;

/** canvas2d_backend.rs, "Deviation": a NaN visible range paints the cleared frame and nothing
 *  else; the original runs its loops with NaN bounds (drawing nothing) but still opens the pane
 *  clips and strokes the price lines. Accept exactly that: the Rust stream is the three frame-start
 *  calls, the original starts with the same three and then holds only clip scaffolding, strokes and the
 *  drawing-slice composites (`drawImage`; the module doc names only the price lines, but the early return
 *  skips the slices too). */
const SCAFFOLDING = new Set(['save', 'restore', 'beginPath', 'rect', 'clip', 'globalAlpha', 'lineWidth', 'lineJoin', 'lineCap', 'strokeStyle', 'fillStyle', 'setLineDash', 'moveTo', 'lineTo', 'stroke', 'fill', 'drawImage']);
/** Whether the view's time range leaves what a JavaScript Date can hold (or is not a number). */
function outsideDateRange(coords) {
    const tr = coords.visibleTimeRange();
    return !(Math.abs(tr.from) <= 8.64e15 && Math.abs(tr.to) <= 8.64e15);
}

function reconcileNanRange(original, rust) {
    if (!original.nanRange || rust.length !== 3 || original.length <= 3) return original;
    const head = original.slice(0, 3);
    if (JSON.stringify(head) !== JSON.stringify(rust)) return original;
    if (!original.slice(3).every((o) => SCAFFOLDING.has(o[0]))) return original;
    documentedDeviations += 1;
    return head;
}

/** format/dates.rs: a time value that is not a valid Date reads "Invalid Date" in the port; the
 *  original's English path (no throw there) prints the NaN fields instead:
 *   - the crosshair's time chip: "undefined NaN undefined 'NaN" (+ " NaN:NaN"). The chip is sized from
 *     its text, so the original's chip (fillRect and the centred fillText) is rebuilt around the port's
 *     text with the shared fixed text metric;
 *   - the time axis' tick labels: "undefined NaN" (day), "undefined" (month), "NaN" (year), "NaN:NaN"
 *     (clock). Only in a frame whose visible time range leaves the valid Date range (|t| > 8.64e15),
 *     and only for centred text, which is how the axis writes them. */
const GARBAGE_STAMP = /^undefined NaN undefined 'NaN/;
const GARBAGE_TICK = /^(?:undefined|NaN)(?:[ :]'?(?:undefined|NaN))*(?: [AP]M)?$/;
function reconcileInvalidDate(original) {
    let font = '10px sans-serif';
    let align = 'start';
    let touched = false;
    const out = original.map((o) => (o.slice ? o.slice() : o));
    out.invalidTimeRange = original.invalidTimeRange;
    for (let i = 0; i < out.length; i++) {
        const o = out[i];
        if (o[0] === 'font') font = o[1];
        if (o[0] === 'textAlign') align = o[1];
        if (o[0] !== 'fillText') continue;
        if (GARBAGE_STAMP.test(o[1])) {
            const rectAt = (() => {
                for (let k = i - 1; k >= 0 && k >= i - 6; k--) if (out[k][0] === 'fillRect') return k;
                return -1;
            })();
            if (rectAt < 0) continue;
            const [rx, ry, w, h] = out[rectAt].slice(1).map((v) => (v === null ? Number.NaN : v)); // a recorded non-finite number is null
            const back = (v) => (Number.isNaN(v) ? null : v);
            const text = 'Invalid Date';
            const newW = recordedTextMetrics(text, font).width + 8;
            const centre = rx + w / 2; // a time chip is centred on its bar
            out[rectAt] = ['fillRect', back(centre - newW / 2), back(ry), newW, back(h)];
            out[i] = ['fillText', text, back(centre), o[3]];
            touched = true;
        } else if (original.invalidTimeRange && align === 'center' && GARBAGE_TICK.test(o[1])) {
            out[i] = ['fillText', 'Invalid Date', o[2], o[3]];
            touched = true;
        }
    }
    if (touched) documentedDeviations += 1;
    return out;
}

/** format/dates.rs: "Where the original would throw on an invalid date (a non-English Intl call, or a NaN
 *  time), the text here reads Invalid Date". In the English path with a locale-driven clock the original's
 *  `Intl.DateTimeFormat#format` throws `RangeError: Invalid time value` mid-frame. What it painted before
 *  the throw must be a prefix of the port's stream, and the port must then have written "Invalid Date". */
function reconcileInvalidTimeThrow(original, rust) {
    const err = original.threw;
    if (!err) return original;
    if (!(err instanceof RangeError && /Invalid time value/.test(err.message))) throw err;
    const prefix = JSON.stringify(original);
    if (JSON.stringify(rust.slice(0, original.length)) !== prefix) throw err;
    if (!rust.slice(original.length).some((o) => o[0] === 'fillText' && o[1] === 'Invalid Date')) throw err;
    documentedDeviations += 1;
    return rust.slice(); // accepted: the rest is the port's documented "Invalid Date" text
}

// ── frames: backend, backdrop, chrome, crosshair ──
dump.frames.forEach((sc, n) => {
    const spec = sc.spec;
    scenarios += 1;
    const guard = (name, run) => {
        try {
            let ops = run();
            ops = reconcileInvalidTimeThrow(ops, sc.ops[name]);
            ops = reconcileInvalidDate(reconcileNanRange(ops, sc.ops[name]));
            if (process.env.SERIES_SHOW === `${n}:${name}`) console.log(JSON.stringify({ original: ops, rust: sc.ops[name] }));
            count(name, check(ops, sc.ops[name], `frame ${n} ${name} (${dump.datasets[spec.dataset].name}, ${spec.scene.priceStyle})`, true), ops.length);
        } catch (err) {
            count(name, false, 0);
            failures.push(`frame ${n} ${name}: the original threw ${err?.stack ?? err}`);
        }
    };
    guard('backend', () => {
        const { scene, coords, theme } = buildFrame(spec);
        const ctx = newContext(spec);
        const backend = new Canvas2dBackend();
        backend.mount(fakeCanvas(ctx, spec));
        backend.modelAlpha = spec.alphas.model;
        backend.candleBodyAlpha = spec.alphas.body;
        backend.candleStructureAlpha = spec.alphas.structure;
        backend.candleBodyScale = spec.alphas.scale;
        backend.render(scene, coords, theme);
        const ops = ctx.toJSON();
        const vr = coords.visibleLogicalRange();
        ops.nanRange = Number.isNaN(vr.from) || Number.isNaN(vr.to);
        return ops;
    });
    // The backdrop and the chrome share one tick source, as in the app.
    const ticks = new PriceAxisTickSource();
    guard('backdrop', () => {
        const { scene, coords, theme } = buildFrame(spec);
        const ctx = newContext(spec);
        const backdrop = new BackdropRenderer(ticks);
        backdrop.mount(fakeCanvas(ctx, spec));
        backdrop.render(scene, coords, theme, spec.gridAlpha);
        return ctx.toJSON();
    });
    guard('chrome', () => {
        const { scene, coords, theme } = buildFrame(spec);
        const ctx = newContext(spec);
        const chrome = new ChromeRenderer(ticks);
        chrome.mount(fakeCanvas(ctx, spec));
        let threw = null;
        try {
            withClock(spec.clock, () => chrome.render(scene, coords, theme, spec.surface ?? undefined));
        } catch (err) {
            threw = err;
        }
        const ops = ctx.toJSON();
        ops.threw = threw;
        ops.invalidTimeRange = outsideDateRange(coords);
        return ops;
    });
    guard('crosshair', () => {
        const { scene, coords, theme } = buildFrame(spec);
        const ctx = newContext(spec);
        const cross = new CrosshairRenderer();
        cross.mount(fakeCanvas(ctx, spec));
        let threw = null;
        try {
            cross.render(scene, coords, theme, spec.separatorHoverY ?? null, spec.external ?? null);
        } catch (err) {
            threw = err;
        }
        const ops = ctx.toJSON();
        ops.threw = threw;
        ops.invalidTimeRange = outsideDateRange(coords);
        check(plain(cross.shadeRightArea), sc.shade, `frame ${n} crosshair shade area`);
        return ops;
    });
});

// ── the timeline-mark lane, directly ──
dump.marks.forEach((sc, n) => {
    const spec = sc.spec;
    scenarios += 1;
    try {
        const bars = barsOf(spec.dataset);
        const coords = new CoordinateSystem();
        coords.setSize(spec.width, spec.height, spec.dpr);
        coords.setBars(bars.map((b) => b.time));
        coords.setViewport({ barSpacing: spec.viewport.barSpacing, rightOffset: spec.viewport.rightOffset });
        const layout = layoutMarkLane({
            marks: spec.marks,
            groups: spec.groups,
            hidden: (g) => spec.hiddenGroups.includes(g),
            barTimes: bars.map((b) => b.time),
            intervalMs: coords.barInterval,
            xOf: (bar) => coords.logicalToX(bar),
            axisY: spec.height,
            dataW: spec.width,
            expanded: spec.expanded,
        });
        const iconCalls = [];
        const icons = {
            get: (icon, ink, px, dpr) => {
                iconCalls.push([icon, ink, px, dpr]);
                return spec.iconsReady ? {} : null;
            },
        };
        const ctx = new RecordingContext({ width: spec.width * spec.dpr, height: spec.height * spec.dpr });
        paintMarkLane(ctx, layout, {
            axisY: spec.height,
            background: spec.colors.background,
            stemColor: spec.colors.stem,
            fontFamily: spec.colors.fontFamily,
            dpr: spec.dpr,
            icons,
            hoverKey: spec.hoverKey,
            hoverSince: spec.hoverSince,
            activeKey: spec.activeKey,
            flashKey: spec.flashKey,
            nowMs: spec.nowMs,
        });
        const ops = ctx.toJSON();
        const glyphs = layout.glyphs.map((g) => ({ key: g.cluster.key, x: g.x, y: g.y, size: g.size, stack: g.stack, depth: g.depth, decked: g.decked, marks: g.cluster.marks.length }));
        const ok = [check(glyphs, sc.glyphs, `marks ${n} layout`), check(ops, sc.ops, `marks ${n} paint_mark_lane`, true), check(iconCalls, sc.iconCalls, `marks ${n} icon requests`)].every(Boolean);
        count('paint_mark_lane', ok, ops.length);
    } catch (err) {
        count('paint_mark_lane', false, 0);
        failures.push(`marks ${n}: the original threw ${err?.stack ?? err}`);
    }
});

// ── the volume and VPVR layers ──
function runLayer(name, list, Renderer, layerData) {
    list.forEach((sc, n) => {
        const spec = sc.spec;
        scenarios += 1;
        try {
            const { scene, coords, theme } = buildFrame(spec);
            const layer = layerData(spec.inputs);
            const ctx = newContext(spec);
            const renderer = new Renderer();
            renderer.mount(fakeCanvas(ctx, spec));
            const pane = scene.panes.get('price');
            let ok = check(plain(layer), sc.layer, `${name} ${n} layer data`);
            let total = 0;
            spec.frames.forEach((fr, k) => {
                coords.setViewport({ barSpacing: fr.barSpacing, rightOffset: fr.rightOffset });
                let bars = scene.bars;
                if (fr.mutateLast && bars.length > 0) {
                    bars = bars.slice();
                    bars[bars.length - 1] = { ...bars[bars.length - 1], high: fr.mutateLast.high, close: fr.mutateLast.close, volume: fr.mutateLast.volume };
                }
                const start = ctx.ops.length;
                let threw = null;
                try {
                    renderer.render({
                        bars,
                        data: fr.noData ? null : layer,
                        visible: fr.visible !== false,
                        coords,
                        scale: pane.scale,
                        bounds: { top: fr.top, height: fr.boundsHeight },
                        theme,
                        fillPane: fr.fillPane === true,
                    });
                } catch (err) {
                    // The original crashes mid-frame (a NaN visible range makes it read bars[NaN]); what it painted
                    // up to there is still compared, the crash itself is reported as a difference, and the frames
                    // after it still run (the throw leaves the renderer's memo untouched).
                    threw = err;
                }
                const ops = JSON.parse(JSON.stringify(ctx.ops.slice(start)));
                total += ops.length;
                const label = `${name} ${n} frame ${k} (${dump.datasets[spec.dataset].name})`;
                ok = check(ops, sc.frames[k], label, true) && ok;
                // render/vpvr.rs, "Deviation": with a NaN visible range the original reads bars[NaN] and
                // throws a TypeError; the port returns after clearing. Accepted only for that error on
                // such a range; what the original painted before the throw was compared just above.
                const range = coords.visibleLogicalRange();
                const nanRange = Number.isNaN(range.from) || Number.isNaN(range.to);
                if (threw && threw instanceof TypeError && nanRange) {
                    documentedDeviations += 1;
                } else if (threw) {
                    ok = false;
                    failures.push(`${label}: the original THREW (${threw.message}); the Rust port returned after clearing the canvas (${sc.frames[k].length} calls)`);
                }
            });
            count(name, ok, total);
        } catch (err) {
            count(name, false, 0);
            failures.push(`${name} ${n}: the original threw ${err?.stack ?? err}`);
        }
    });
}
runLayer('VolumeRenderer', dump.volume, VolumeRenderer, volumeLayerData);
runLayer('VpvrRenderer', dump.vpvr, VpvrRenderer, vpvrLayerData);

// ── the pure painters with hand-made geometry ──
dump.paintVolume.forEach((sc, n) => {
    const spec = sc.spec;
    scenarios += 1;
    try {
        const bars = barsOf(spec.dataset);
        const g = spec.geom;
        const ctx = new RecordingContext();
        paintVolume(ctx, bars, { i0: g.i0, i1: g.i1, xOf: (i) => g.right - (bars.length - 1 - i) * g.spacing, halfW: g.halfW, bottomY: g.bottomY, maxH: g.maxH, maxVol: g.maxVol }, { up: g.up, down: g.down });
        const ops = ctx.toJSON();
        count('paintVolume', check(ops, sc.ops, `paintVolume ${n}`, true), ops.length);
    } catch (err) {
        count('paintVolume', false, 0);
        failures.push(`paintVolume ${n}: the original threw ${err?.stack ?? err}`);
    }
});

const priceCoords = new CoordinateSystem();
dump.paintVpvr.forEach((sc, n) => {
    const spec = sc.spec;
    scenarios += 1;
    try {
        const g = spec.geom;
        const scale = { ...g.scale };
        const bounds = { top: g.top, height: g.height };
        const ctx = new RecordingContext();
        paintVpvr(ctx, spec.profile, { rightX: g.rightX, maxW: g.maxW, yOf: (p) => priceCoords.priceToY(p, scale, bounds) }, { upColor: g.up, downColor: g.down, showPoc: g.showPoc, pocColor: g.poc });
        const ops = ctx.toJSON();
        count('paintVpvr', check(ops, sc.ops, `paintVpvr ${n}`, true), ops.length);
    } catch (err) {
        count('paintVpvr', false, 0);
        failures.push(`paintVpvr ${n}: the original threw ${err?.stack ?? err}`);
    }
});

console.log(`${scenarios} scenarios, ${calls} canvas calls, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}, ${documentedDeviations} documented deviations normalized`);
for (const [name, p] of Object.entries(perPainter)) console.log(`  ${name}: ${p.scenarios} runs, ${p.calls} calls, ${p.failed} with differences`);
if (failures.length > 0) {
    console.log(`${failures.length} difference(s):`);
    for (const f of failures.slice(0, 25)) console.log(`  ${f.length > 1100 ? `${f.slice(0, 1100)}…` : f}`);
    process.exit(1);
}
console.log('The Rust port paints what the original paints.');
