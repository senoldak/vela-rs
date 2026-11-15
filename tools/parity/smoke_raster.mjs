// Smoke test of vela-web's DOM overlay rasterizer (`src/raster.rs`, the port of the original's
// `dom-raster.ts`) in Node. The rasterizer is a script inlined in the package; this loads it
// from `pkg/snippets` and runs it over a small hand-built DOM with a recording 2D context:
// backgrounds and borders with their radius, text at its measured rect in its computed font,
// hidden and zero-size boxes skipped, opacity multiplied down, SVG icons through their
// viewBox, transformed SVG elements skipped, the collect order of the overlay passes, and a
// failing element never throwing. It does NOT run a browser.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_raster.mjs pkg

import { readdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

let failed = 0;
const check = (name, ok, detail = '') => {
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok || !detail ? '' : ` (${detail})`}`);
    if (!ok) failed += 1;
};

const pkg = resolve(process.argv[2] ?? 'pkg');
const snippets = join(pkg, 'snippets');
let modulePath = null;
for (const dir of readdirSync(snippets)) {
    for (const file of readdirSync(join(snippets, dir))) {
        const p = join(snippets, dir, file);
        if (readFileSync(p, 'utf8').includes('export function vela_raster_overlay')) modulePath = p;
    }
}
if (!modulePath) {
    console.error('vela_raster_overlay is not in the package');
    process.exit(1);
}

// A tiny DOM: elements carry `rect`, `style` (the computed style), children and text nodes.
class HTMLElementFake {
    constructor(tag, cs = {}, rect = { left: 0, top: 0, width: 0, height: 0 }) {
        this.tagName = tag.toUpperCase();
        this.cs = cs;
        this.rect = rect;
        this.childNodes = [];
        this.attrs = {};
        this.ownerDocument = { defaultView: win, createRange: () => makeRange() };
    }
    get children() {
        return this.childNodes.filter((n) => n.nodeType === 1);
    }
    add(...kids) {
        this.childNodes.push(...kids);
        return this;
    }
    getBoundingClientRect() {
        return this.rect;
    }
    hasAttribute(n) {
        return n in this.attrs;
    }
    getAttribute(n) {
        return this.attrs[n] ?? null;
    }
}
HTMLElementFake.prototype.nodeType = 1;
class SVGSVGFake extends HTMLElementFake {}
class ImgFake extends HTMLElementFake {}
const win = {
    HTMLElement: HTMLElementFake,
    SVGSVGElement: SVGSVGFake,
    HTMLImageElement: ImgFake,
    getComputedStyle: (el) => ({
        display: 'block',
        visibility: 'visible',
        opacity: '1',
        backgroundColor: 'transparent',
        borderTopLeftRadius: '0px',
        borderTopWidth: '0px',
        borderTopStyle: 'none',
        borderTopColor: 'transparent',
        color: '#d1d4dc',
        fontStyle: 'normal',
        fontWeight: '400',
        fontSize: '12px',
        fontFamily: 'sans-serif',
        fill: 'none',
        stroke: 'none',
        strokeWidth: '0',
        fillOpacity: '1',
        strokeOpacity: '1',
        ...el.cs,
    }),
};
let nextRange = null;
const makeRange = () => ({
    selectNodeContents(node) {
        nextRange = node.rect;
    },
    getBoundingClientRect: () => nextRange,
});
const text = (value, rect) => ({ nodeType: 3, textContent: value, rect, ownerDocument: { createRange: () => makeRange() } });

const recorder = () => {
    const calls = [];
    const ctx = new Proxy(
        { calls },
        {
            get(target, prop) {
                if (prop === 'calls') return calls;
                if (prop === 'roundRect') return (...a) => calls.push(['roundRect', ...a]);
                return (...a) => calls.push([String(prop), ...a]);
            },
            set(target, prop, value) {
                calls.push(['set:' + String(prop), value]);
                return true;
            },
        },
    );
    return ctx;
};
const names = (ctx) => ctx.calls.map((c) => c[0]);
const find = (ctx, name) => ctx.calls.filter((c) => c[0] === name);

globalThis.Path2D = class {
    constructor(d) {
        this.d = d;
    }
    rect() {}
    moveTo() {}
    lineTo() {}
    arc() {}
    ellipse() {}
    closePath() {}
};

const raster = await import(pathToFileURL(modulePath).href);

// ── a legend chip: a tinted rounded box with a text ──
{
    const label = text('RSI 14', { left: 112, top: 30, width: 40, height: 14 });
    const chip = new HTMLElementFake('div', { backgroundColor: 'rgba(41, 98, 255, 0.4)', borderTopLeftRadius: '4px', borderTopWidth: '1px', borderTopStyle: 'solid', borderTopColor: '#2962ff' }, { left: 110, top: 28, width: 60, height: 18 }).add(label);
    const ctx = recorder();
    raster.vela_raster_overlay(ctx, chip, 100, 20, 2);
    const scale = find(ctx, 'scale')[0];
    check('the canvas is scaled by the export dpr', scale && scale[1] === 2 && scale[2] === 2);
    const rr = find(ctx, 'roundRect');
    check('the background is a rounded rect at its on-screen rect relative to the canvas origin', rr[0] && rr[0].join() === 'roundRect,10,8,60,18,4', rr.map((r) => r.join()).join(' | '));
    check('the background is filled with its computed color', ctx.calls.some((c, i) => c[0] === 'set:fillStyle' && c[1] === 'rgba(41, 98, 255, 0.4)' && ctx.calls[i + 1]?.[0] === 'beginPath'));
    check('the border is stroked inset by half its width', rr[1] && rr[1].join() === 'roundRect,10.5,8.5,59,17,3.5', rr.map((r) => r.join()).join(' | '));
    const ft = find(ctx, 'fillText')[0];
    check('the text is drawn at its measured rect, vertically centered', ft && ft[1] === 'RSI 14' && ft[2] === 12 && ft[3] === 17, JSON.stringify(ft));
    check('the text uses the computed font and color', ctx.calls.some((c) => c[0] === 'set:font' && c[1] === 'normal 400 12px sans-serif') && ctx.calls.some((c) => c[0] === 'set:fillStyle' && c[1] === '#d1d4dc'));
    check('every save has its restore', find(ctx, 'save').length === find(ctx, 'restore').length);
}

// ── '50%' reads as a circle; a percentage radius resolves against the box ──
{
    const dot = new HTMLElementFake('span', { backgroundColor: '#26a69a', borderTopLeftRadius: '50%' }, { left: 0, top: 0, width: 10, height: 10 });
    const ctx = recorder();
    raster.vela_raster_overlay(ctx, dot, 0, 0, 1);
    check("a '50%' radius is half the box", find(ctx, 'roundRect')[0]?.join() === 'roundRect,0,0,10,10,5');
}

// ── hidden, invisible, zero-size and transparent things draw nothing ──
{
    const hidden = new HTMLElementFake('div', { display: 'none', backgroundColor: '#fff' }, { left: 0, top: 0, width: 10, height: 10 }).add(text('x', { left: 0, top: 0, width: 5, height: 5 }));
    const invisible = new HTMLElementFake('div', { visibility: 'hidden', backgroundColor: '#fff' }, { left: 0, top: 0, width: 10, height: 10 });
    const empty = new HTMLElementFake('div', { backgroundColor: '#fff' }, { left: 0, top: 0, width: 0, height: 10 });
    const clear = new HTMLElementFake('div', { opacity: '0' , backgroundColor: '#fff' }, { left: 0, top: 0, width: 10, height: 10 });
    const rgba0 = new HTMLElementFake('div', { backgroundColor: 'rgba(0, 0, 0, 0)' }, { left: 0, top: 0, width: 10, height: 10 });
    for (const [name, el] of [['display:none', hidden], ['visibility:hidden', invisible], ['zero-size', empty], ['opacity 0', clear], ['transparent', rgba0]]) {
        const ctx = recorder();
        raster.vela_raster_overlay(ctx, el, 0, 0, 1);
        check(`${name} draws nothing`, !names(ctx).some((n) => ['fill', 'fillText', 'stroke'].includes(n)), names(ctx).join(','));
    }
}

// ── opacity multiplies down the subtree (the 5% watermark stays 5%) ──
{
    const inner = text('BTC', { left: 0, top: 0, width: 30, height: 10 });
    const root = new HTMLElementFake('div', { opacity: '0.5' }, { left: 0, top: 0, width: 100, height: 50 }).add(new HTMLElementFake('span', { opacity: '0.1', color: '#fff' }, { left: 0, top: 0, width: 30, height: 10 }).add(inner));
    const ctx = recorder();
    raster.vela_raster_overlay(ctx, root, 0, 0, 1);
    const alpha = ctx.calls.find((c) => c[0] === 'set:globalAlpha');
    check('opacity multiplies down', alpha && Math.abs(alpha[1] - 0.05) < 1e-9, JSON.stringify(alpha));
}

// ── an SVG icon: scaled from its viewBox, stroke from computed style, transforms skipped ──
{
    const path = new HTMLElementFake('path', { stroke: '#fff', strokeWidth: '2', strokeLinecap: 'round' });
    path.attrs.d = 'M0 0L24 24';
    const moved = new HTMLElementFake('path', { stroke: '#fff', strokeWidth: '2' });
    moved.attrs.d = 'M1 1';
    moved.attrs.transform = 'rotate(45)';
    const circle = new HTMLElementFake('circle', { fill: '#f00' });
    circle.attrs.cx = '12';
    circle.attrs.cy = '12';
    circle.attrs.r = '4';
    const svg = new SVGSVGFake('svg', {}, { left: 50, top: 40, width: 12, height: 12 }).add(path, moved, circle);
    svg.viewBox = { baseVal: { x: 0, y: 0, width: 24, height: 24 } };
    const host = new HTMLElementFake('span', {}, { left: 50, top: 40, width: 12, height: 12 }).add(svg);
    const ctx = recorder();
    raster.vela_raster_overlay(ctx, host, 0, 0, 1);
    check('the SVG is translated to its box and scaled by box / viewBox', find(ctx, 'translate')[0]?.join() === 'translate,50,40' && find(ctx, 'scale').some((c) => c[1] === 0.5 && c[2] === 0.5));
    check('a stroked path strokes with its computed width and cap', find(ctx, 'stroke').length === 1 && ctx.calls.some((c) => c[0] === 'set:lineWidth' && c[1] === 2) && ctx.calls.some((c) => c[0] === 'set:lineCap' && c[1] === 'round'));
    check('a circle is filled; the transformed path is skipped', find(ctx, 'fill').length === 1 && find(ctx, 'stroke').length === 1);
}

// ── never throws ──
{
    const broken = new HTMLElementFake('div', { backgroundColor: '#fff' }, { left: 0, top: 0, width: 10, height: 10 });
    broken.getBoundingClientRect = () => {
        throw new Error('boom');
    };
    const ctx = recorder();
    let threw = false;
    try {
        raster.vela_raster_overlay(ctx, broken, 0, 0, 1);
    } catch {
        threw = true;
    }
    check('a failing element does not lose the screenshot', !threw && find(ctx, 'save').length === find(ctx, 'restore').length);
    let threw2 = false;
    try {
        raster.vela_raster_overlay(recorder(), { ownerDocument: null }, 0, 0, 1);
    } catch {
        threw2 = true;
    }
    check('an element without a window is skipped', !threw2);
}

// ── the overlay passes: under = marked "under"; over = legend columns, then the other marked ──
{
    const mk = (attrs) => {
        const e = new HTMLElementFake('div');
        e.attrs = attrs;
        return e;
    };
    const wm = mk({ 'data-vela-screenshot': 'under' });
    const sl = mk({ 'data-vela-screenshot': '1' });
    const lg = mk({ 'data-vela-pane': 'p0' });
    const container = {
        querySelectorAll: (sel) => (sel === '[data-vela-screenshot]' ? [sl, wm] : sel === '[data-vela-pane]' ? [lg] : []),
    };
    check('the under pass is the watermark only', raster.vela_raster_collect(container, false).join() === [wm].join() && raster.vela_raster_collect(container, false)[0] === wm && raster.vela_raster_collect(container, false).length === 1);
    const over = raster.vela_raster_collect(container, true);
    check('the over pass is the legends, then the status line and the mark', over.length === 2 && over[0] === lg && over[1] === sl);
}

if (failed > 0) {
    console.error(`\n${failed} raster check(s) FAILED`);
    process.exit(1);
}
console.log('\nsmoke_raster: all checks passed (a hand-built DOM; no browser was run)');
