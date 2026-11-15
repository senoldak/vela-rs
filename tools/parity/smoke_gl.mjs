// Smoke test of the WebGL2 geometry backend's browser half (vela-web `gl.rs`, the GL data
// canvas of BrowserHost, the input wiring that follows the data canvas) in Node, against the
// fake DOM with its fake `webgl2` context switched on.
//
// The fake context (fake_dom.mjs, built on recording_gl.mjs) records every call and answers
// what the backend asks: shaders compile, programs link (unless a check turns that off), the
// context is lost only when told. This checks that the chart paints through GL, that the
// canvases are sized and stacked right, that input reaches whichever canvas is the data
// canvas, and that every fallback path (no WebGL2, a program that does not link, a context
// that cannot be restored) lands on a fresh 2D canvas. It does NOT run a real WebGL
// implementation: shader compilation, real rasterisation, real context loss events and
// premultiplied-alpha compositing are not exercised.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_gl.mjs pkg
//
// Exits 1 when a check fails.

import { check, clock, container, doc, frames, glControl, loadPackage, makeBars, options, painted, pileOf, pump, state, tick } from './fake_dom.mjs';

options.webgl = true;
const wasm = await loadPackage('smoke_gl.mjs');
const failures = () => state.failures;

const draws = (gl) => gl.calls('drawArrays').length;
const lastViewport = (gl) => gl.calls('viewport').at(-1);
const isData = (c) => c.style.getPropertyValue('pointer-events') !== 'none';

function build(width = 900, height = 560, options = undefined, theme = 'dark') {
    const el = container(width, height);
    const chart = options === undefined ? new wasm.VelaChart(el, theme) : wasm.VelaChart.withOptions(el, theme, options);
    chart.setBars(makeBars(400));
    chart.addVolume();
    pump();
    const { plot, canvases } = pileOf(el);
    return { el, chart, plot, canvases, data: canvases.find(isData) };
}

const wheel = (data, deltaY = -240) => data.dispatch('wheel', { clientX: 300, clientY: 200, deltaX: 0, deltaY, deltaMode: 0, shiftKey: false, ctrlKey: false, metaKey: false });

// ── 1. paints through GL ──
{
    const { el, chart, plot, canvases, data } = build();
    const gl = data.gl;
    check('auto picks WebGL2 when the browser has it', chart.backend() === 'webgl2');
    check('the data canvas holds a webgl2 context', gl && data.contextKind === 'webgl2' && glControl.contexts.length === 1);
    check('the context is asked for the original\'s attributes', JSON.stringify(data.glAttrs) === JSON.stringify({ alpha: true, antialias: false, depth: false, stencil: false, premultipliedAlpha: true }), JSON.stringify(data.glAttrs));
    check('the pile keeps its seven canvases and order', canvases.length === 7 && plot.children[1] === data && plot.children[0] === canvases[0] && plot.children[2] === canvases[2], `${canvases.length}`);
    check('the data canvas still takes the pointer, the rest do not', canvases.filter(isData).length === 1 && data.style.getPropertyValue('touch-action') === 'none');
    check('every canvas is sized to the plot in device px', canvases.every((c) => c.width === 900 && c.height === 560 && c.style.getPropertyValue('width') === '900px'), `${data.width}x${data.height}`);
    check('programs were linked and linking was checked', gl.calls('linkProgram').length >= 1 && gl.calls('getProgramParameter').length >= 1, `${gl.calls('linkProgram').length} programs`);
    check('geometry went through draw calls', draws(gl) > 0, `${draws(gl)} draws`);
    check('the GL viewport is the backing size', JSON.stringify(lastViewport(gl)) === JSON.stringify(['viewport', 0, 0, 900, 560]), JSON.stringify(lastViewport(gl)));
    check('the 2D context of the data canvas stays untouched', !painted(data) && data.ctx.ops.length === 0);
    const others = canvases.filter((c) => c !== data);
    check('the other canvases of the pile still paint in 2D', others.every(painted), `blank: ${others.filter((c) => !painted(c)).length}`);
    check('keyboard attributes sit on the GL data canvas', data.tabIndex === 0 && data.getAttribute('role') === 'application');
    check('only the data canvas has a WebGL context', canvases.filter((c) => c.gl).length === 1);

    // Resize.
    gl.clearOps();
    el.rect = { left: 0, top: 0, width: 700, height: 420 };
    chart.resize();
    pump();
    check('a resize resizes the GL canvas', data.width === 700 && data.height === 420 && data.style.getPropertyValue('width') === '700px');
    check('a resize moves the GL viewport', JSON.stringify(lastViewport(gl)) === JSON.stringify(['viewport', 0, 0, 700, 420]), JSON.stringify(lastViewport(gl)));
    check('a resize repaints through GL', draws(gl) > 0);

    // Pan and zoom repaint.
    const [spacing0] = chart.viewport();
    gl.clearOps();
    const e = wheel(data);
    pump();
    const [spacing1] = chart.viewport();
    check('the wheel reaches the GL data canvas and zooms', e.defaultPrevented === true && spacing1 > spacing0 * 1.2, `${spacing0.toFixed(2)} -> ${spacing1.toFixed(2)}`);
    check('a zoom repaints through GL', draws(gl) > 0);
    const [, offsetBefore] = chart.viewport();
    gl.clearOps();
    const base = { pointerId: 7, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false };
    data.dispatch('pointerdown', { ...base, clientX: 400, clientY: 200, timeStamp: clock });
    for (let i = 1; i <= 8; i += 1) {
        tick(8);
        data.dispatch('pointermove', { ...base, clientX: 400 + i * 25, clientY: 200, timeStamp: clock });
    }
    data.dispatch('pointerup', { ...base, clientX: 600, clientY: 200, timeStamp: clock });
    pump();
    check('a drag pans and repaints through GL', chart.viewport()[1] < offsetBefore - 1 && draws(gl) > 0);

    chart.destroy();
    const created = (name) => gl.calls(name).length;
    check('destroy leaves no listener on the GL canvas', data.listeners.length === 0 && doc.listeners.length === 0, `canvas ${data.listeners.length}, document ${doc.listeners.length}`);
    check('destroy deletes the GL objects the chart created', created('deleteProgram') >= 1 && created('deleteBuffer') >= 1 && created('deleteVertexArray') >= 1, `programs ${created('deleteProgram')}, buffers ${created('deleteBuffer')}`);
    check('destroy releases the context itself', created('loseContext') === 1 && gl.lost === true);
    check('destroy empties the container', el.children.length === 0);
}

// ── 2. drawings behind the series composite as a texture ──
{
    const { chart, canvases, data } = build();
    const gl = data.gl;
    const base = { pointerId: 5, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false };
    const click = (x, y) => {
        data.dispatch('pointerdown', { ...base, clientX: x, clientY: y, timeStamp: clock });
        data.dispatch('pointerup', { ...base, clientX: x, clientY: y, timeStamp: clock });
        pump();
    };
    chart.armDrawingTool('trendline');
    click(200, 200);
    click(500, 260);
    const docs = JSON.parse(chart.exportDrawings());
    check('a trend line was drawn on the GL data canvas', docs.drawings.length === 1, `${docs.drawings.length}`);
    docs.drawings[0].zIndex = 1e6; // over the series stack: the drawings canvas, no slice
    chart.clearDrawings();
    gl.clearOps();
    chart.importDrawings(JSON.stringify(docs));
    pump();
    check('a drawing over the series needs no texture', gl.calls('texImage2D').length === 0 && draws(gl) > 0);
    docs.drawings[0].zIndex = -5; // behind the series stack
    chart.clearDrawings();
    gl.clearOps();
    chart.importDrawings(JSON.stringify(docs));
    pump();
    const uploads = gl.calls('texImage2D').filter((op) => op[6] === 'canvas' || op.includes('canvas'));
    check('a drawing behind the series is uploaded from its slice canvas', uploads.length >= 1, `${uploads.length} uploads`);
    check('the slice texture is drawn', draws(gl) > 0 && gl.calls('bindTexture').length > 0);
    check('the slice canvas painted in 2D', canvases.length === 7);
    chart.destroy();
}

// ── 3. canvas2d on request ──
{
    const { chart, canvases, data } = build(900, 560, { backend: 'canvas2d' });
    check('backend canvas2d keeps the 2D path', chart.backend() === 'canvas2d' && !data.gl && data.contextKind === '2d' && painted(data));
    check('the 2D data canvas sits in the same stacking slot', canvases.length === 7 && canvases[1] === data);
    chart.destroy();
    const before = glControl.contexts.length;
    const el = container(400, 300);
    check('an unknown backend is an error', (() => {
        try {
            wasm.VelaChart.withOptions(el, 'dark', { backend: 'vulkan' });
            return false;
        } catch (e) {
            return String(e.message ?? e).includes('unknown backend');
        }
    })());
    check('a bad glow is an error', (() => {
        try {
            wasm.VelaChart.withOptions(el, 'dark', { glow: -1 });
            return false;
        } catch (e) {
            return String(e.message ?? e).includes('options.glow');
        }
    })());
    check('a rejected option opens no GL context', glControl.contexts.length === before);
    const forced = build(900, 560, { backend: 'webgl2' });
    check('backend webgl2 uses GL', forced.chart.backend() === 'webgl2' && !!forced.data.gl);
    forced.chart.destroy();
    const defaults = build(900, 560, {});
    check('empty options mean auto', defaults.chart.backend() === 'webgl2');
    defaults.chart.destroy();
    check('the constructor after withOptions is back to auto', (() => {
        const again = build();
        const ok = again.chart.backend() === 'webgl2';
        again.chart.destroy();
        return ok;
    })());
}

// ── 4. no WebGL2 in this browser ──
{
    glControl.available = false;
    const { chart, data } = build();
    check('without WebGL2 the chart keeps canvas2d', chart.backend() === 'canvas2d' && !data.gl && painted(data));
    const [s0] = chart.viewport();
    wheel(data);
    pump();
    check('input works on the 2D canvas', chart.viewport()[0] > s0 * 1.2);
    chart.destroy();
    glControl.available = true;
}

// ── 5. a program that does not link ──
{
    glControl.linkOk = false;
    const before = glControl.contexts.length;
    const { el, chart, plot, canvases, data } = build();
    const glCanvas = glControl.contexts.at(-1).canvas;
    glControl.linkOk = true;
    check('a failed link falls back to canvas2d', chart.backend() === 'canvas2d' && glControl.contexts.length === before + 1);
    check('the tainted GL canvas is gone, a fresh 2D one took its place', !glCanvas.parentNode && glCanvas !== data && !data.gl && data.contextKind === '2d' && painted(data));
    check('the pile is whole and ordered again', canvases.length === 7 && plot.children[1] === data, `${canvases.length}`);
    check('the failed context was released', glControl.contexts.at(-1).calls('loseContext').length === 1);
    check('the fresh canvas is sized and keyboard-focusable', data.width === 900 && data.height === 560 && data.tabIndex === 0 && data.getAttribute('role') === 'application');
    const [s0] = chart.viewport();
    const e = wheel(data);
    pump();
    check('input works on the fresh canvas (wheel zooms)', e.defaultPrevented === true && chart.viewport()[0] > s0 * 1.2);
    chart.destroy();
    check('destroy cleans the fallback canvas', data.listeners.length === 0 && el.children.length === 0 && doc.listeners.length === 0);
}

// ── 6. context lost, then restored ──
{
    const { chart, plot, canvases, data } = build();
    const gl = data.gl;
    const linksBefore = gl.calls('linkProgram').length;
    gl.lost = true;
    const lost = data.dispatch('webglcontextlost', {});
    check('webglcontextlost is prevented so the context can come back', lost.defaultPrevented === true);
    gl.clearOps();
    wheel(data);
    pump();
    check('nothing is drawn while the context is lost', draws(gl) === 0 && chart.backend() === 'webgl2', `${draws(gl)} draws`);
    gl.lost = false;
    data.dispatch('webglcontextrestored', {});
    pump();
    check('a restore rebuilds the programs', gl.calls('linkProgram').length > 0 && linksBefore >= 1, `${gl.calls('linkProgram').length} links`);
    check('a restore repaints through GL', draws(gl) > 0 && chart.backend() === 'webgl2');
    check('the data canvas is the same one', plot.children[1] === data && canvases.length === 7);
    const [s0] = chart.viewport();
    wheel(data);
    pump();
    check('input still works after a restore', chart.viewport()[0] > s0 * 1.05);
    chart.destroy();
}

// ── 7. a restore that fails ──
{
    const { el, chart, plot, canvases, data } = build();
    const gl = data.gl;
    const [s0] = chart.viewport();
    gl.lost = true;
    data.dispatch('webglcontextlost', {});
    gl.lost = false;
    glControl.linkOk = false;
    data.dispatch('webglcontextrestored', {});
    glControl.linkOk = true;
    const fresh = plot.children.filter((c) => c.tagName === 'CANVAS' && isData(c));
    check('a failed restore falls back to canvas2d', chart.backend() === 'canvas2d');
    check('the data canvas was replaced by a 2D one', fresh.length === 1 && fresh[0] !== data && !fresh[0].gl && fresh[0].contextKind === '2d', `${fresh.length}`);
    const next = fresh[0];
    check('the new canvas sits in the data slot of the pile', plot.children[1] === next && plot.children.filter((c) => c.tagName === 'CANVAS').length === 7);
    check('the new canvas is sized and painted', next.width === 900 && next.height === 560 && painted(next));
    check('keyboard attributes moved to the new canvas', next.tabIndex === 0 && next.getAttribute('role') === 'application');
    pump(); // the retired GL canvas lets go of its listeners
    check('the old canvas lost its listeners (input moved)', data.listeners.length === 0, `${data.listeners.length} left`);
    const e = wheel(next);
    pump();
    check('the wheel on the new canvas zooms', e.defaultPrevented === true && chart.viewport()[0] > s0 * 1.2);
    const base = { pointerId: 9, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false };
    const [, o0] = chart.viewport();
    next.dispatch('pointerdown', { ...base, clientX: 400, clientY: 200, timeStamp: clock });
    for (let i = 1; i <= 6; i += 1) {
        tick(8);
        next.dispatch('pointermove', { ...base, clientX: 400 + i * 25, clientY: 200, timeStamp: clock });
    }
    next.dispatch('pointerup', { ...base, clientX: 550, clientY: 200, timeStamp: clock });
    pump();
    check('a drag on the new canvas pans', chart.viewport()[1] < o0 - 1);
    const k = next.dispatch('keydown', { key: 'ArrowLeft', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false });
    check('the keyboard works on the new canvas', k.defaultPrevented === true);
    const stale = data.dispatch('webglcontextrestored', {});
    check('a stale event of the old canvas is ignored', stale && chart.backend() === 'canvas2d');
    chart.destroy();
    check('destroy cleans up after the swap', next.listeners.length === 0 && data.listeners.length === 0 && el.children.length === 0 && doc.listeners.length === 0);
    check('the canvases list is empty after destroy', canvases.every((c) => !c.parentNode));
}

// ── 8. glow ──
{
    const plain = build();
    const glowing = build(900, 560, { glow: 1.5 });
    const countFrame = (b) => {
        b.data.gl.clearOps();
        wheel(b.data, -120);
        pump();
        return draws(b.data.gl);
    };
    const [d0, d1] = [countFrame(plain), countFrame(glowing)];
    check('glow > 0 adds the glow pass', d1 > d0 && d0 > 0, `${d0} -> ${d1} draws`);
    plain.chart.setGlow(1.5);
    pump();
    const d2 = countFrame(plain);
    check('setGlow turns it on later', d2 > d0, `${d0} -> ${d2} draws`);
    plain.chart.setGlow(0);
    pump();
    const d3 = countFrame(plain);
    check('setGlow(0) turns it off again', d3 < d2 && d3 < d0 * 2, `${d2} -> ${d3} draws`);
    plain.chart.destroy();
    glowing.chart.destroy();
}

// ── 9. the facade chart ──
{
    const el = container(900, 560);
    const app = wasm.VelaApp.withOptions(el, 'dark', 'DEMO', '1h', makeBars(300), { backend: 'webgl2' });
    await app.ready();
    for (let i = 0; i < 6; i += 1) {
        pump();
        await Promise.resolve();
    }
    const { canvases } = pileOf(el);
    const data = canvases.find(isData);
    check('VelaApp.withOptions picks the backend', app.backend() === 'webgl2' && !!data.gl && draws(data.gl) > 0);
    app.setGlow(1);
    const plain = new wasm.VelaApp(container(400, 300), 'dark', 'DEMO', '1h', makeBars(100));
    await plain.ready();
    check('VelaApp defaults to auto', plain.backend() === 'webgl2');
    plain.destroy();
    app.destroy();
    check('the facade chart tears down', el.children.length === 0 && doc.listeners.length === 0);
}

check('frames settle', frames.size === 0, `${frames.size} pending`);
if (failures() > 0) {
    console.log(`${failures()} check(s) failed.`);
    process.exit(1);
}
console.log('vela-web WebGL2 wiring behaves as expected against the fake DOM and fake GL (no real WebGL was run).');
