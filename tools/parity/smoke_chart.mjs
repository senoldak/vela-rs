// Smoke test of vela-web's chart wiring (BrowserHost, input listeners, observers and the
// JavaScript API) in Node, against a minimal fake DOM.
//
// The fake document, elements and canvases implement only what the wiring touches; every
// canvas hands out a RecordingContext (see recording_canvas.mjs), and requestAnimationFrame,
// ResizeObserver and matchMedia are driven by hand. This checks the DOM structure, the paint
// of every canvas of the pile, the input paths and the teardown. It does NOT run a browser:
// real layout, real pointer capture, real painting and real devicePixelContentBox reports
// are not exercised.
//
// The drawings scenario at the end (toolbar, claims protocol, inline editor, quick settings bar,
// persistence) runs on both the direct chart and the facade chart.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_chart.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, FakeNode, check, clock, container, doc, dprWatchers, frames, loadPackage, makeBars, observers, painted, pileOf, pump, state, tick } from './fake_dom.mjs';

const wasm = await loadPackage('smoke_chart.mjs');
const failures = () => state.failures;

// ── VelaChart ──

const el = container(900, 560);
const chart = new wasm.VelaChart(el, 'dark');
const bars = makeBars(400);
chart.setBars(bars);
chart.addVolume();
const emaId = chart.addIndicator('ema'); // the inputs argument is optional
const rsiId = chart.addIndicator('rsi', undefined);
const frameCount = pump();
check('frames settle', frames.size === 0, `${frameCount} frames`);
check('bars are on the chart', chart.barCount() === 400);
check('indicators got ids', emaId.startsWith('ema') && rsiId.startsWith('rsi'), `${emaId}, ${rsiId}`);

const { wrapper, plot, canvases } = pileOf(el);
check('wrapper and plot are built inside the container', wrapper && plot && el.children.length === 1);
check('wrapper has dir-less pinned layout styles', wrapper.style.getPropertyValue('position') === 'relative' && wrapper.style.getPropertyValue('overflow') === 'hidden');
check('the pile has seven canvases', canvases.length === 7, `${canvases.length}`);
check('every canvas is pinned left to right and absolutely placed', canvases.every((c) => c.dir === 'ltr' && c.style.getPropertyValue('position') === 'absolute'));
check('only the data canvas takes pointer events', canvases.filter((c) => c.style.getPropertyValue('pointer-events') !== 'none').length === 1);
const data = canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
check('canvases are sized to the plot in device px', canvases.every((c) => c.width === 900 && c.height === 560 && c.style.getPropertyValue('width') === '900px'), `${data.width}x${data.height}`);
const blank = canvases.filter((c) => !painted(c));
check('every canvas of the pile painted, the drawings layer included', blank.length === 0, `blank: ${blank.map((c) => canvases.indexOf(c)).join(',')}`);
check('canvas order: backdrop, data, volume, vpvr, chrome, drawings, shade-right, cursor, overlay', plot.children.length === 9 && plot.children[0] === canvases[0] && plot.children[6].className === 'vela-shade-right', plot.children.map((c) => c.tagName + (c.className ? '.' + c.className : '')).join(' '));

const mark = wrapper.all((n) => n.className === 'vela-attribution')[0];
check('the Vela attribution mark is there', mark && mark.getAttribute('href')?.startsWith('https://velacharts.dev/') && mark.style.getPropertyValue('display') !== 'none' && mark.innerHTML === '' && mark.children.length === 2);
check('the attribution is positioned above the time axis', mark && Number.parseFloat(mark.style.getPropertyValue('bottom')) >= 32 && mark.style.getPropertyValue('left') === '12px', `bottom ${mark?.style.getPropertyValue('bottom')}`);
check('layout variables are published on the container', el.style.getPropertyValue('--vela-scale-gutter') !== '' && el.style.getPropertyValue('--vela-bottom-gutter') !== '' && el.style.getPropertyValue('--vela-toolbar-gutter') === '0px');
check('the container background follows the theme', wrapper.style.getPropertyValue('background') === '#151619');
check('keyboard makes the data canvas focusable with an ARIA live region', data.tabIndex === 0 && data.getAttribute('role') === 'application' && wrapper.all((n) => n.getAttribute('aria-live') === 'polite').length === 1);
check('the attribution tooltip and the keyboard help read in English first', mark.getAttribute('title') === 'Charting by Vela' && String(data.getAttribute('aria-label')).startsWith('Interactive price chart'));
chart.setLocale('tr');
check('Turkish: the attribution tooltip and the keyboard help follow setLocale', mark.getAttribute('title') === 'Grafik altyapısı: Vela' && mark.getAttribute('aria-label') === 'Grafik altyapısı: Vela' && String(data.getAttribute('aria-label')).startsWith('Etkileşimli fiyat grafiği'), `${mark.getAttribute('title')} | ${data.getAttribute('aria-label')}`);
chart.setLocale('en');
check('and back to English', mark.getAttribute('title') === 'Charting by Vela');

const [spacing0, offset0] = chart.viewport();

// Crosshair: a hover paints the cursor canvas.
const cursorCanvas = canvases[6];
cursorCanvas.ctx.ops = [];
data.dispatch('pointermove', { clientX: 300, clientY: 200, pointerId: 1, pointerType: 'mouse', button: 0, timeStamp: clock });
pump();
check('a pointer move paints the cursor canvas', painted(cursorCanvas));
check('the cursor over an axis strip is a resize cursor', (() => {
    data.dispatch('pointermove', { clientX: 880, clientY: 200, pointerId: 1, pointerType: 'mouse', button: 0, timeStamp: clock });
    return data.style.getPropertyValue('cursor') === 'ns-resize';
})());
data.dispatch('pointerleave', {});
pump();

// Wheel: zoom.
const wheel = data.dispatch('wheel', { clientX: 300, clientY: 200, deltaX: 0, deltaY: -240, deltaMode: 0, shiftKey: false, ctrlKey: false, metaKey: false });
pump();
const [spacing1] = chart.viewport();
check('the wheel is consumed (preventDefault)', wheel.defaultPrevented === true);
check('a wheel notch up zooms in', spacing1 > spacing0 * 1.2, `${spacing0.toFixed(2)} -> ${spacing1.toFixed(2)}`);

// Pointer drag: pan.
const [, offsetBefore] = chart.viewport();
const rangeBeforeDrag = chart.visibleRange();
data.dispatch('pointerdown', { clientX: 400, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
for (let i = 1; i <= 8; i += 1) {
    tick(8);
    data.dispatch('pointermove', { clientX: 400 + i * 25, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
}
data.dispatch('pointerup', { clientX: 600, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, timeStamp: clock });
pump();
const [, offsetAfter] = chart.viewport();
check('dragging right reveals earlier bars (right offset decreases)', offsetAfter < offsetBefore - 1, `${offsetBefore.toFixed(2)} -> ${offsetAfter.toFixed(2)}`);
const range1 = chart.visibleRange();
check('the visible range moved back in time', range1[0] < rangeBeforeDrag[0] && range1[1] < rangeBeforeDrag[1], `${new Date(rangeBeforeDrag[0]).toISOString()} -> ${new Date(range1[0]).toISOString()}`);

// Shift + wheel pans through time.
const [, offsetShift0] = chart.viewport();
data.dispatch('wheel', { clientX: 300, clientY: 200, deltaX: 0, deltaY: 120, shiftKey: true, ctrlKey: false, metaKey: false });
pump();
check('shift + wheel pans', chart.viewport()[1] !== offsetShift0);

// Double-click on the data area is routed (no pane to maximize here, so it only has to not throw).
data.dispatch('dblclick', { clientX: 300, clientY: 100, button: 0 });
pump();

// Keyboard.
const live = wrapper.all((n) => n.getAttribute('aria-live') === 'polite')[0];
const keyEvent = data.dispatch('keydown', { key: 'ArrowLeft', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false });
pump();
check('an arrow key is handled and announced', keyEvent.defaultPrevented === true && live.textContent.includes('Open'), live.textContent);
check('an unmapped key is left alone', data.dispatch('keydown', { key: 'q', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false }).defaultPrevented !== true);

// Touch: two-finger pinch zooms.
const [pinchBefore] = chart.viewport();
const touch = (type, id, x, y) => data.dispatch(type, { clientX: x, clientY: y, pointerId: id, pointerType: 'touch', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
touch('pointerdown', 11, 400, 200);
touch('pointerdown', 12, 440, 200);
for (let i = 1; i <= 5; i += 1) {
    tick(8);
    touch('pointermove', 12, 440 + i * 20, 200);
}
touch('pointerup', 12, 540, 200);
touch('pointerup', 11, 400, 200);
pump();
check('a pinch spreads the bars', chart.viewport()[0] > pinchBefore * 1.5, `${pinchBefore.toFixed(2)} -> ${chart.viewport()[0].toFixed(2)}`);

// Live update: the last bar changes, then a new bar appends.
const last = bars[bars.length - 1];
data.ctx.ops = [];
chart.updateBar(last.time, last.open, last.high + 5, last.low, last.close + 3, last.volume + 10);
pump();
check('updating the last bar repaints the data canvas', painted(data) && chart.barCount() === 400);
chart.updateBar(last.time + 3_600_000, last.close, last.close + 2, last.close - 2, last.close + 1, 900);
pump();
check('a newer bar appends', chart.barCount() === 401);

// Resize through the observer.
const observer = observers.find((o) => o.connected);
check('a ResizeObserver watches the wrapper and the plot device box', observer && observer.targets.length === 2 && observer.targets[1].options?.box === 'device-pixel-content-box');
el.rect = { left: 0, top: 0, width: 700, height: 420 };
observer.cb([{ target: plot, devicePixelContentBoxSize: [{ inlineSize: 700, blockSize: 420 }] }]);
pump();
check('a resize re-sizes every canvas of the pile', canvases.every((c) => c.width === 700 && c.height === 420), `${data.width}x${data.height}`);
check('a resize repaints', painted(data) && painted(canvases[4]));

// devicePixelRatio change through the media query.
globalThis.devicePixelRatio = 2;
const armed = dprWatchers.at(-1);
armed.listeners[0]();
pump();
check('a devicePixelRatio change re-sizes the backing stores', canvases.every((c) => c.width === 1400 && c.height === 840 && c.style.getPropertyValue('width') === '700px'), `${data.width}x${data.height}`);
globalThis.devicePixelRatio = 1;
dprWatchers.at(-1).listeners[0]();
pump();

// Theme switch.
chart.setTheme('light');
pump();
check('the light theme repaints the background', wrapper.style.getPropertyValue('background') !== '#151619', wrapper.style.getPropertyValue('background'));
const ink = mark.style.getPropertyValue('color');
check('the attribution ink follows the plot background', ink === '#000000', ink);

// Typed-array input.
chart.setBarsTyped(Float64Array.from(bars, (b) => b.time), Float64Array.from(bars, (b) => b.open), Float64Array.from(bars, (b) => b.high), Float64Array.from(bars, (b) => b.low), Float64Array.from(bars, (b) => b.close), Float64Array.from(bars, (b) => b.volume));
pump();
check('setBarsTyped replaces the bars', chart.barCount() === 400);

// Indicator removal.
check('an indicator can be removed', chart.removeIndicator(rsiId) === true && chart.removeIndicator(rsiId) === false);
pump();
check('remove an unknown type errors', (() => {
    try {
        chart.addIndicator('nope', undefined);
        return false;
    } catch (e) {
        return String(e.message ?? e).includes('unknown indicator');
    }
})());

// Teardown.
const listenersBefore = data.listeners.length;
chart.destroy();
check('destroy removes the DOM', el.children.length === 0 && el.style.getPropertyValue('--vela-scale-gutter') === '');
check('destroy removes the data canvas listeners', listenersBefore > 0 && data.listeners.length === 0, `${listenersBefore} -> ${data.listeners.length}`);
check('destroy disconnects the observer', !observer.connected && doc.defaultView.listeners.length === 0);
check('destroy leaves no frame pending', frames.size === 0);
chart.destroy(); // idempotent

// ── VelaApp (the Vela facade over an in-memory feed) ──

const el2 = container(800, 500);
const app = new wasm.VelaApp(el2, 'dark', 'DEMO', '1h', makeBars(300));
await app.ready();
for (let i = 0; i < 20; i += 1) await Promise.resolve();
app.addIndicator('sma');
app.addIndicator('rsi', undefined);
for (let i = 0; i < 20; i += 1) await Promise.resolve();
pump();
const appPile = pileOf(el2);
const appBlank = appPile.canvases.filter((c) => !painted(c));
check('the facade chart paints its canvases', appPile.canvases.length === 7 && appBlank.length <= 1, `blank: ${appBlank.length}`);
const appBars = makeBars(300);
const lastApp = appBars[appBars.length - 1];
check('a live tick reaches the engine', app.tick(lastApp.time, lastApp.open, lastApp.high + 1, lastApp.low, lastApp.close + 1, 5000) === true);
for (let i = 0; i < 20; i += 1) await Promise.resolve();
pump();
const appData = appPile.canvases[1];
appData.dispatch('wheel', { clientX: 300, clientY: 200, deltaX: 0, deltaY: -240, shiftKey: false, ctrlKey: false, metaKey: false });
const appSpacing = app.viewport()[0];
pump();
check('input works on the facade chart', app.viewport()[0] > appSpacing, `${appSpacing.toFixed(2)} -> ${app.viewport()[0].toFixed(2)}`);
app.destroy();
check('the facade destroy removes the DOM', el2.children.length === 0 && appData.listeners.length === 0);

// ── user drawings: toolbar, claims protocol, editor, bar, persistence ──

const macrotask = () => new Promise((resolve) => setTimeout(resolve, 5));
async function settle() {
    for (let i = 0; i < 6; i += 1) {
        pump();
        await Promise.resolve();
    }
    await macrotask();
    pump();
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

async function drawingsScenario(label, build) {
    const ck = (name, ok, detail) => check(`${label}: ${name}`, ok, detail);
    const host = container(900, 560);
    const chart = await build(host);
    await settle();
    const { wrapper: w, plot: p, canvases: cs } = pileOf(host);
    const dataC = cs.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
    const drawingsCanvas = cs[5];
    let off = 0; // the toolbar gutter, once it is docked
    const base = { pointerId: 5, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false };
    const mouse = (type, x, y, extra = {}) => dataC.dispatch(type, { ...base, clientX: x + off, clientY: y, timeStamp: clock, ...extra });
    const click = (x, y, extra = {}) => {
        mouse('pointerdown', x, y, extra);
        mouse('pointerup', x, y, extra);
    };
    const key = (k, extra = {}) => dataC.dispatch('keydown', { key: k, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...extra });
    const docs = () => JSON.parse(chart.exportDrawings()).drawings;
    const byClass = (root, cls) => root.all((n) => n.className === cls);
    const toolbar = () => byClass(w, 'vela-dtb')[0];
    const bars = () => w.all((n) => n.className.split(' ').includes('vela-dpop'));

    // The API surface.
    ck('the tool catalog lists the trend line', wasm.VelaChart.drawingToolTypes().includes('trendline') && wasm.VelaApp.drawingToolTypes().includes('text'));
    ck('an unknown tool key is an error', (() => {
        try {
            chart.armDrawingTool('nope');
            return false;
        } catch (e) {
            return String(e.message ?? e).includes('unknown drawing tool');
        }
    })());
    ck('importing text that is not JSON is an error', (() => {
        try {
            chart.importDrawings('not json');
            return false;
        } catch (e) {
            return String(e.message ?? e).includes('not JSON');
        }
    })());

    // The toolbar starts hidden, and docks on request.
    let tb = toolbar();
    ck('the toolbar exists but is hidden by default', tb && tb.style.getPropertyValue('display') === 'none' && host.style.getPropertyValue('--vela-toolbar-gutter') === '0px');
    chart.showDrawingToolbar(true);
    await settle();
    tb = toolbar();
    const buttons = tb.all((n) => n.tagName === 'BUTTON');
    ck('showDrawingToolbar docks the bar and reserves its gutter', tb.style.getPropertyValue('display') === 'flex' && host.style.getPropertyValue('--vela-toolbar-gutter') === '44px' && tb.style.getPropertyValue('width') === '44px');
    ck('the bar has buttons, each with an accessible name', buttons.length >= 12 && buttons.every((b) => (b.getAttribute('aria-label') ?? '') !== ''), `${buttons.length} buttons`);
    const enNames = tb.all((n) => n.tagName === 'BUTTON').map((b) => b.getAttribute('aria-label'));
    ck('the fixed toolbar labels are English first', ['Cursor', 'Measure', 'Magnet snap: Off', 'Stay in drawing mode', 'Collapse toolbar'].every((l) => enNames.includes(l)), enNames.join(' | '));
    chart.setLocale('tr');
    await settle();
    tb = toolbar();
    const trNames = tb.all((n) => n.tagName === 'BUTTON').map((b) => b.getAttribute('aria-label'));
    ck('Turkish: the fixed toolbar labels follow setLocale', ['İmleç', 'Ölçüm', 'Silgi (silmek için tıklayın veya sürükleyin)', 'Mıknatıs modu: Kapalı', 'Çizim modunda kal', 'Araç çubuğunu daralt'].every((l) => trNames.includes(l)), trNames.join(' | '));
    ck('Turkish: the chevrons read "daha fazla"', trNames.some((n) => n.endsWith('daha fazla')), trNames.join(' | '));
    chart.setLocale('en');
    await settle();
    tb = toolbar();
    ck('the cursor starts active', tb.all((n) => n.getAttribute('aria-label') === 'Cursor')[0].getAttribute('aria-pressed') === 'true');
    off = 44;
    ck('the plot is narrowed by the gutter', dataC.width === 856, `${dataC.width}`);

    // The mode buttons mirror the controller: measure, cursor, magnet, stay-in-drawing-mode, stars.
    const byLabel = (root, text) => root.all((n) => n.getAttribute('aria-label') === text)[0];
    const pressed = (el) => el?.getAttribute('aria-pressed');
    byLabel(tb, 'Measure').dispatch('click', { detail: 1 });
    await settle();
    ck('the Measure button arms the ruler (and the cursor lets go)', pressed(byLabel(tb, 'Measure')) === 'true' && pressed(byLabel(tb, 'Cursor')) === 'false');
    byLabel(tb, 'Cursor').dispatch('click', { detail: 1 });
    await settle();
    ck('the cursor button leaves it', pressed(byLabel(tb, 'Measure')) === 'false' && pressed(byLabel(tb, 'Cursor')) === 'true');
    const magnetOf = () => tb.all((n) => (n.getAttribute('aria-label') ?? '').startsWith('Magnet snap:'))[0];
    magnetOf().dispatch('click', { detail: 1 });
    await settle();
    ck('the magnet icon toggles the last-used strength on', magnetOf().getAttribute('aria-label') === 'Magnet snap: Weak' && pressed(magnetOf()) === 'true');
    magnetOf().dispatch('click', { detail: 1 });
    await settle();
    ck('and off again', magnetOf().getAttribute('aria-label') === 'Magnet snap: Off' && pressed(magnetOf()) === 'false');
    byLabel(tb, 'Stay in drawing mode').dispatch('click', { detail: 1 });
    await settle();
    ck('the stay button reflects stay-in-drawing-mode', pressed(byLabel(tb, 'Stay in drawing mode')) === 'true');
    byLabel(tb, 'Stay in drawing mode').dispatch('click', { detail: 1 });
    await settle();
    ck('and clears it', pressed(byLabel(tb, 'Stay in drawing mode')) === 'false');

    // A flyout star favorites a tool in place, without closing the menu.
    byClass(tb, 'vela-dtb-arrow')[0].dispatch('click', { detail: 1 });
    const star = byClass(byClass(w, 'vela-dtb-flyout')[0], 'vela-dtb-star')[0];
    star.dispatch('click', { detail: 1 });
    await settle();
    ck('a star favorites the tool and the flyout stays open', star.className.includes('vela-fav') && pressed(star) === 'true' && byClass(w, 'vela-dtb-flyout').length === 1);
    star.dispatch('click', { detail: 1 });
    await settle();
    ck('and unstars it', !star.className.includes('vela-fav'));
    byClass(w, 'vela-dtb-flyout')[0].dispatch('keydown', { key: 'Escape' });
    await settle();
    ck('Escape closes a flyout', byClass(w, 'vela-dtb-flyout').length === 0);

    // Arm the trend line from its flyout.
    const arrows = byClass(tb, 'vela-dtb-arrow');
    arrows[0].dispatch('click', { detail: 1 });
    const flyout = byClass(w, 'vela-dtb-flyout')[0];
    const item = flyout?.all((n) => n.tagName === 'BUTTON' && n.getAttribute('aria-label') === 'Trend Line')[0];
    ck('a chevron opens a flyout listing the group tools', flyout && item && flyout.all((n) => n.className.startsWith('vela-dtb-star')).length > 5);
    item.dispatch('click', { detail: 1 });
    await settle();
    ck('picking a tool in the flyout arms it and closes the flyout', chart.drawingTool() === 'trendline' && byClass(w, 'vela-dtb-flyout').length === 0);

    // Two clicks place it, and the chart does not pan.
    const vp0 = chart.viewport();
    click(300, 200);
    mouse('pointermove', 500, 120);
    pump();
    click(500, 120);
    await settle();
    ck('two clicks on the plot create a trend line', chart.drawingCount() === 1 && docs()[0].type === 'trendline', `${chart.drawingCount()} drawings`);
    ck('placing did not pan the chart', same(chart.viewport(), vp0), `${vp0} -> ${chart.viewport()}`);
    ck('a one-shot tool disarms after placing', chart.drawingTool() === undefined);
    ck('the new drawing opens the quick bar', bars().length === 1 && bars()[0].all((n) => n.getAttribute('aria-label') === 'Delete').length === 1);

    // Escape closes the bar; a click on the drawing selects it and brings the bar back.
    key('Escape');
    await settle();
    ck('Escape closes the bar', bars().length === 0);
    click(400, 160);
    await settle();
    ck('a click on a drawing selects it and shows the bar', bars().length === 1);
    ck('that click did not pan', same(chart.viewport(), vp0));

    // The bar: color, lock. The line color button opens a popover with the native swatch.
    let bar = bars()[0];
    bar.all((n) => n.getAttribute('aria-label') === 'Line color')[0].dispatch('click', { detail: 1 });
    const color = w.all((n) => n.className === 'vela-ctl-swatch')[0];
    color.value = '#ff0000';
    color.dispatch('change', {});
    ck('the bar color control patches the line color', docs()[0].style.lineColor === '#ff0000', docs()[0].style.lineColor);
    bar = bars()[0];
    bar.all((n) => n.getAttribute('aria-label') === 'Lock')[0].dispatch('click', { detail: 1 });
    await settle();
    const lock = () => bars()[0].all((n) => n.getAttribute('aria-label') === 'Lock')[0];
    ck('the bar lock button locks the drawing', docs()[0].locked === true && lock().getAttribute('aria-pressed') === 'true');
    lock().dispatch('click', { detail: 1 });
    await settle();
    ck('and unlocks it again', docs()[0].locked === false && lock().getAttribute('aria-pressed') === 'false');

    // Drag the drawing: anchors move, the chart does not pan.
    await macrotask(); // the bar arms its outside-press listener one tick after it opens
    const anchors0 = docs()[0].anchors;
    mouse('pointerdown', 400, 160);
    for (let i = 1; i <= 6; i += 1) {
        tick(8);
        mouse('pointermove', 400 + i * 5, 160 + i * 5);
    }
    mouse('pointerup', 430, 190);
    await settle();
    ck('dragging a drawing moves its anchors', !same(docs()[0].anchors, anchors0));
    ck('and does not pan the chart', same(chart.viewport(), vp0), `${vp0} -> ${chart.viewport()}`);

    // Delete, undo.
    key('Delete');
    await settle();
    ck('Delete removes the selected drawing', chart.drawingCount() === 0 && bars().length === 0);
    key('z', { ctrlKey: true });
    await settle();
    ck('Ctrl+Z restores it', chart.drawingCount() === 1);
    key('z', { ctrlKey: true, shiftKey: true });
    await settle();
    ck('and Ctrl+Shift+Z redoes the delete', chart.drawingCount() === 0);
    key('z', { ctrlKey: true });
    await settle();
    ck('the history keys were taken by the drawings layer', chart.drawingCount() === 1);

    // A drag on empty space still pans.
    key('Escape');
    await settle();
    const [, panBefore] = chart.viewport();
    mouse('pointerdown', 150, 400);
    for (let i = 1; i <= 8; i += 1) {
        tick(8);
        mouse('pointermove', 150 + i * 15, 400);
    }
    mouse('pointerup', 270, 400);
    await settle();
    ck('a drag on empty space still pans', chart.viewport()[1] < panBefore - 1, `${panBefore.toFixed(2)} -> ${chart.viewport()[1].toFixed(2)}`);

    // Shift-drag starts the measure ruler and does not pan.
    const vpMeasure = chart.viewport();
    drawingsCanvas.ctx.ops = [];
    mouse('pointerdown', 250, 350, { shiftKey: true });
    for (let i = 1; i <= 6; i += 1) {
        tick(8);
        mouse('pointermove', 250 + i * 20, 350 - i * 8, { shiftKey: true });
    }
    pump();
    ck('Shift-drag on the empty plot paints the measure ruler', painted(drawingsCanvas));
    mouse('pointerup', 370, 302, { shiftKey: true });
    await settle();
    ck('and does not pan the chart', same(chart.viewport(), vpMeasure), `${vpMeasure} -> ${chart.viewport()}`);
    ck('measuring leaves no drawing behind', chart.drawingCount() === 1);
    click(100, 450); // the next press clears the finished ruler
    await settle();

    // The text tool: the toolbar icon arms it, a click places it, the inline editor edits it.
    const textIcon = tb.all((n) => n.className === 'vela-dtb-icon' && n.getAttribute('aria-label') === 'Text')[0];
    textIcon.dispatch('click', { detail: 1 });
    await settle();
    ck('a group icon arms the group tool', chart.drawingTool() === 'text');
    click(300, 300);
    await settle();
    const area = p.all((n) => n.tagName === 'TEXTAREA')[0];
    ck('placing text opens the inline editor, focused', area && doc.activeElement === area && area.style.getPropertyValue('position') === 'absolute' && area.style.getPropertyValue('z-index') === '24');
    area.value = 'Hi';
    area.setSelectionRange(2, 2);
    area.dispatch('keydown', { key: 'Enter', ctrlKey: false, metaKey: false, isComposing: false });
    ck('a plain Enter inserts a line break itself', area.value === 'Hi\n');
    area.value = 'Hello Vela';
    area.dispatch('input', {});
    const chord = area.dispatch('keydown', { key: 'Enter', ctrlKey: true, metaKey: false, isComposing: false });
    await settle();
    ck('typing then Ctrl+Enter commits the text', docs().some((d) => d.type === 'text' && JSON.stringify(d).includes('Hello Vela')) && p.all((n) => n.tagName === 'TEXTAREA').length === 0 && chord.defaultPrevented === true);
    ck('the editor keystrokes stayed out of the chart', chart.drawingCount() === 2);

    // The cursor over the drawings layer: a pointer over the line, the default beside it.
    const hits = [];
    for (let x = 250; x < 800; x += 5) {
        mouse('pointermove', x, 190);
        if (dataC.style.getPropertyValue('cursor') === 'pointer') hits.push(x);
    }
    ck('the cursor is a pointer over a drawing and not beside it', hits.length >= 2 && hits.length < 12, `pointer at x = ${hits[0]}..${hits.at(-1)}`);
    const lineX = hits[Math.floor(hits.length / 2)] ?? 550;
    // Right click cancels an armed tool, and swallows the context menu for that press only.
    chart.armDrawingTool('box');
    await settle();
    mouse('pointerdown', 300, 300, { button: 2 });
    ck('right-click disarms the tool', chart.drawingTool() === undefined);
    ck('and vetoes the context menu once', dataC.dispatch('contextmenu', {}).defaultPrevented === true && dataC.dispatch('contextmenu', {}).defaultPrevented !== true);

    // Middle click deletes the drawing under the cursor.
    const countBefore = chart.drawingCount();
    const mid = mouse('pointerdown', lineX, 190, { button: 1 });
    await settle();
    ck('middle-click deletes the drawing under the cursor', chart.drawingCount() === countBefore - 1 && mid.defaultPrevented === true);
    ck('and vetoes the autoscroll companion', dataC.dispatch('mousedown', { button: 1 }).defaultPrevented === true);
    key('z', { ctrlKey: true });
    await settle();
    ck('the delete is one undo step', chart.drawingCount() === countBefore);

    // Collapse, theme.
    const collapse = tb.all((n) => n.className.includes('vela-dtb-collapse'))[0];
    collapse.dispatch('click', { detail: 1 });
    await settle();
    ck('collapsing narrows the bar and the gutter', tb.style.getPropertyValue('width') === '16px' && host.style.getPropertyValue('--vela-toolbar-gutter') === '16px');
    collapse.dispatch('click', { detail: 1 });
    await settle();
    ck('expanding restores both', tb.style.getPropertyValue('width') === '44px' && host.style.getPropertyValue('--vela-toolbar-gutter') === '44px');
    ck('the widgets read the chrome theme variables', w.style.getPropertyValue('--vela-chrome-text-color') !== '');

    // Persistence: export, destroy, rebuild, import.
    const json = chart.exportDrawings();
    const exported = JSON.parse(json).drawings;
    chart.destroy();
    ck('destroy removes the widgets and their listeners', host.children.length === 0 && dataC.listeners.length === 0 && doc.listeners.length === 0, `document listeners ${doc.listeners.length}`);
    const host2 = container(900, 560);
    const chart2 = await build(host2);
    await settle();
    const n = chart2.importDrawings(json);
    await settle();
    const back = JSON.parse(chart2.exportDrawings()).drawings;
    ck('export, destroy, new chart, import round-trips', n === exported.length && same(back.map((d) => [d.id, d.type, d.anchors, d.style]), exported.map((d) => [d.id, d.type, d.anchors, d.style])), `${n} drawings`);
    if (!same(back, exported)) console.log(JSON.stringify(back).slice(0, 700), '|', JSON.stringify(exported).slice(0, 700));
    chart2.destroy();
    ck('the second chart tears down too', host2.children.length === 0 && doc.listeners.length === 0);
}

await drawingsScenario('direct chart', async (host) => {
    const c = new wasm.VelaChart(host, 'dark');
    c.setBars(makeBars(300));
    c.addVolume();
    return c;
});
await drawingsScenario('facade chart', async (host) => {
    const a = new wasm.VelaApp(host, 'dark', 'DEMO', '1h', makeBars(300));
    await a.ready();
    return a;
});

if (failures() > 0) {
    console.log(`${failures()} check(s) failed.`);
    process.exit(1);
}
console.log('vela-web chart wiring behaves as expected against the fake DOM (no browser was run).');
