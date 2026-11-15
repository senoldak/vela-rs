// Smoke test of vela-web's browser workspace (BrowserWorkspaceHost, the widget root, the
// splitters, persistence and the JavaScript API) in Node, against the minimal fake DOM.
//
// The fake DOM has no layout engine: this script plays one. After every change it reads the
// grid template and each cell's grid-area from the inline styles the host wrote, computes
// the cell boxes, and fires the ResizeObservers by hand. That checks that the host writes
// the right template and placements and that cells follow their box; it does NOT run a
// browser (real CSS grid layout, real pointer capture, real painting and real focus
// behavior are not exercised), and no exchange endpoint is ever called (the in-memory
// provider is the only data source).
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_workspace.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, check, clock, container, doc, frames, loadPackage, observers, options, painted, pump, state, storage, storageControl, tick } from './fake_dom.mjs';

options.captureAncestors = true;
// The fake canvas has no encoder: a PNG data URL naming its backing size stands in.
FakeCanvas.prototype.toDataURL = function toDataURL(type) {
    return `data:${type};size=${this.width}x${this.height}`;
};

const wasm = await loadPackage('smoke_workspace.mjs');

const W = 1000;
const H = 600;
const GAP = 2;
const SYMBOLS = ['AAA', 'BBB', 'CCC', 'DDD'];

// Four deterministic series, different levels, the same time grid.
function bars(seed, level, n = 300, start = Date.UTC(2025, 0, 1), step = 3_600_000) {
    let s = seed;
    const rnd = () => ((s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
    const out = [];
    let price = level;
    for (let i = 0; i < n; i += 1) {
        const open = price;
        const close = open + (rnd() - 0.5) * 2;
        out.push({ time: start + i * step, open, high: Math.max(open, close) + rnd(), low: Math.min(open, close) - rnd(), close, volume: 1000 + Math.floor(rnd() * 500) });
        price = close;
    }
    return out;
}
const seedBars = () => Object.fromEntries(SYMBOLS.map((s, i) => [s, bars(100 + i, 50 + i * 25)]));

// ── a stand-in for layout ──

const tracksOf = (text) => text.split(/\s+/).filter(Boolean).map((t) => Number.parseFloat(t));
const spans = (weights, total) => {
    const content = total - GAP * (weights.length - 1);
    const sum = weights.reduce((a, b) => a + b, 0);
    let at = 0;
    return weights.map((w) => {
        const size = (content * w) / sum;
        const span = [at, size];
        at += size + GAP;
        return span;
    });
};
const cellsOf = (grid) => grid.children.filter((c) => c.className === 'vela-cell');

function layoutGrid(grid, width = W, height = H) {
    grid.rect = { left: 0, top: 0, width, height };
    const cols = spans(tracksOf(grid.style.getPropertyValue('grid-template-columns')), width);
    const rows = spans(tracksOf(grid.style.getPropertyValue('grid-template-rows')), height);
    cellsOf(grid).forEach((cell, i) => {
        const area = cell.style.getPropertyValue('grid-area');
        let box;
        if (area.includes('/')) box = { left: 0, top: 0, width, height };
        else {
            const [x, w] = cols[i % cols.length];
            const [y, h] = rows[Math.floor(i / cols.length)] ?? rows.at(-1);
            box = { left: x, top: y, width: w, height: h };
        }
        cell.rect = box;
    });
}

const macrotask = () => new Promise((resolve) => setTimeout(resolve, 5));
let current = null;
function relayout() {
    if (current) layoutGrid(current.grid());
    for (const o of observers) if (o.connected) o.cb([]);
}
async function settle() {
    for (let round = 0; round < 3; round += 1) {
        for (let i = 0; i < 4; i += 1) {
            pump();
            await Promise.resolve();
        }
        await macrotask();
        relayout();
    }
    pump();
}

const rootOf = (host) => host.children.find((c) => c.className.includes('vela-workspace'));
const cellEls = (ws) => cellsOf(ws.grid());
const cellEl = (ws, id) => cellEls(ws).find((c) => c.getAttribute('data-cell-id') === id);
const pileOf = (cell) => {
    const wrapper = cell.children[0];
    const plot = wrapper.children[0];
    return { wrapper, plot, canvases: plot.children.filter((c) => c instanceof FakeCanvas) };
};
const dataCanvas = (cell) => pileOf(cell).canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
const cursorCanvas = (cell) => pileOf(cell).canvases[6];
const pointer = (target, type, x, y, extra = {}) =>
    target.dispatch(type, { clientX: x, clientY: y, pointerId: 3, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock, ...extra });
const symbolOf = (ws, id) => ws.cell(id).symbol();
const sameSymbol = (a, b) => a.split(':').at(-1) === b;
const totalListeners = (node) => node.listeners.length + node.children.reduce((n, c) => n + totalListeners(c), 0);

// ── a 2x2 workspace ──

const host = container(W, H);
const listenersBefore = totalListeners(host);
const ws = new wasm.VelaWorkspace(host, {
    layout: '4',
    bars: seedBars(),
    theme: 'dark',
    persist: 'vela-smoke-workspace',
    cells: Object.fromEntries(SYMBOLS.map((s) => [s.toLowerCase(), { symbol: s, timeframe: '60' }])),
});
current = ws;
await ws.ready();
await settle();

const root = rootOf(host);
check('the root is built inside the container', root && host.children.length === 1 && root.className.split(' ').includes('vela-workspace'));
check('the root is a regions grid with a layout mode and tokens', root.hasAttribute('data-vela-regions') && root.getAttribute('data-layout') === 'desktop' && root.style.getPropertyValue('--vela-bg') === '#151619' && root.style.getPropertyValue('--vela-border-soft') === '#2a2b30');
check('the stylesheet is injected once, in the chrome layer', doc.head.all((n) => n.id === 'vela-workspace').length === 1 && doc.getElementById('vela-workspace').textContent.includes('@layer vela.chrome'));

const grid = ws.grid();
const REGION_CLASS = {
    topbar: 'vela-widget-topbar', toolbar: 'vela-ws-toolbar', main: 'vela-ws-main', strips: 'vela-ws-strips', bottombar: 'vela-widget-bottombar',
    'strips-below': 'vela-ws-strips-below', mobilebar: 'vela-mobilebar', column: 'vela-ws-column', edge: 'vela-ws-edge',
};
check('the root has the nine named regions with the original class names', ws.regions().length === 9 && ws.regions().every((r) => ws.region(r).className.split(' ').includes(REGION_CLASS[r]) && ws.region(r).getAttribute('data-vela-region') === r), ws.regions().join(','));
check('the regions are children of the root in order, the grid is in main', root.children.length === 9 && ws.region('main').children[0] === grid && grid.className === 'vela-ws-grid');
// The topbar, the bottom bar and the side surfaces mount into their regions now; the regions
// no component docks into are still empty (they collapse to nothing).
check('the regions nothing docks into are empty (they collapse to nothing)', ['strips', 'strips-below', 'column'].every((r) => ws.region(r).children.length === 0));
check('an unknown region is an error', (() => { try { ws.region('nope'); return false; } catch (e) { return String(e.message).includes('unknown region'); } })());

// grid presentation
const cells = cellEls(ws);
check('four cells with their identities', cells.length === 4 && cells.map((c) => c.getAttribute('data-cell-id')).join() === 'aaa,bbb,ccc,ddd', cells.map((c) => c.getAttribute('data-cell-id')).join());
check('each cell is relative and clipped', cells.every((c) => c.style.getPropertyValue('position') === 'relative' && c.style.getPropertyValue('overflow') === 'hidden'));
check('the grid template is 2 columns by 2 rows', grid.style.getPropertyValue('grid-template-columns') === '1fr 1fr' && grid.style.getPropertyValue('grid-template-rows') === '1fr 1fr', `${grid.style.getPropertyValue('grid-template-columns')} / ${grid.style.getPropertyValue('grid-template-rows')}`);
check('the grid is marked multi, not maximized', grid.getAttribute('data-multi') === '1' && !grid.hasAttribute('data-maximized'));
check('the layout list names the registered layouts', ws.layouts().some((l) => l.id === '4' && l.cells === 4) && ws.layout() === '4');

// every cell paints
const bad = cells.filter((c) => pileOf(c).canvases.length !== 7 || !painted(dataCanvas(c)));
check('every cell builds a pile of seven canvases and paints its data canvas', bad.length === 0, `unpainted: ${bad.map((c) => c.getAttribute('data-cell-id')).join(',')}`);
check('each cell chart sits on its own container (BrowserHost)', cells.every((c) => c.children[0].className === 'vela-native'));
check('the cell canvases are sized to the cell box', cells.every((c) => dataCanvas(c).width === Math.round(c.rect.width)), `${dataCanvas(cells[0]).width} vs ${cells[0].rect.width}`);
check('each cell shows its own symbol', SYMBOLS.every((s, i) => sameSymbol(symbolOf(ws, s.toLowerCase()), s)), SYMBOLS.map((s) => symbolOf(ws, s.toLowerCase())).join());
check('each cell has its own legend (the in-chart chrome of a single chart)', cells.every((c) => c.all((n) => String(n.className).split(' ').includes('vela-legend')).length >= 1), `${cells.map((c) => c.all((n) => String(n.className).split(' ').includes('vela-legend')).length).join()}`);

// the attribution mark: one, in the bottom-left cell
const marks = root.all((n) => n.className === 'vela-attribution' && n.style.getPropertyValue('display') !== 'none');
check('exactly one attribution mark is visible, in the bottom-left cell', marks.length === 1 && marks[0].parentNode === cells[2], `${marks.length} marks, parent ${marks[0]?.parentNode?.getAttribute('data-cell-id')}`);
check('the mark links to the project and carries the dark ink', marks[0].getAttribute('href').startsWith('https://velacharts.dev/') && marks[0].style.getPropertyValue('color') === '#ffffff');

// splitter strips
const strips = () => grid.all((n) => n.className === 'vela-ws-splitter');
check('a 2x2 grid has one column strip and one row strip', strips().length === 2 && strips().map((s) => s.getAttribute('data-axis')).sort().join() === 'cols,rows', `${strips().length} strips`);
const colStrip = () => strips().find((s) => s.getAttribute('data-axis') === 'cols');
check('the column strip sits on the vertical seam', Math.abs(Number.parseFloat(colStrip().style.getPropertyValue('left')) - (W / 2 - 4)) <= 2 && colStrip().style.getPropertyValue('cursor') === 'col-resize' && colStrip().style.getPropertyValue('z-index') === '30', colStrip().style.getPropertyValue('left'));

// ── activation ──

check('the first cell is active', ws.activeCell() === 'aaa' && cells[0].getAttribute('data-active') === '1' && cells.filter((c) => c.hasAttribute('data-active')).length === 1);
const activeEvents = [];
ws.on('cell:active', (e) => activeEvents.push(e));
pointer(dataCanvas(cellEl(ws, 'ccc')), 'pointerdown', 100, 400);
pointer(dataCanvas(cellEl(ws, 'ccc')), 'pointerup', 100, 400);
await settle();
check('a pointerdown inside a cell activates it (capture phase on the container)', ws.activeCell() === 'ccc' && activeEvents.length === 1 && activeEvents[0].id === 'ccc' && activeEvents[0].prev === 'aaa');
check('the active styling moves with it', cellEl(ws, 'ccc').getAttribute('data-active') === '1' && !cellEl(ws, 'aaa').hasAttribute('data-active'));
cellEl(ws, 'bbb').dispatch('focusin', {});
await settle();
check('focus inside a cell activates it too', ws.activeCell() === 'bbb');
ws.setActiveCell('aaa');
await settle();
check('setActiveCell moves the ring', cellEl(ws, 'aaa').getAttribute('data-active') === '1' && ws.cell('aaa').isActive() && !ws.cell('bbb').isActive());

// ── the cell handle ──

const aaa = ws.cell('aaa');
check('a cell handle reads and writes the market', aaa.id === 'aaa' && aaa.timeframe() === '60' && aaa.priceStyle() === 'candles' && ws.cell('nope') === undefined);

// ── symbol sync ──

check('the symbol link is off by default', ws.sync('symbol') === false);
aaa.setSymbol('CCC');
await settle();
check('without the link, setSymbol changes only that cell', sameSymbol(symbolOf(ws, 'aaa'), 'CCC') && sameSymbol(symbolOf(ws, 'bbb'), 'BBB') && sameSymbol(symbolOf(ws, 'ddd'), 'DDD'), SYMBOLS.map((s) => symbolOf(ws, s.toLowerCase())).join());
check('turning the link on returns it on', ws.sync('symbol', true) === true && ws.sync('symbol') === true);
await settle();
ws.cell('bbb').setSymbol('DDD');
await settle();
check('with the link on, setSymbol changes every cell', ['aaa', 'bbb', 'ccc', 'ddd'].every((id) => sameSymbol(symbolOf(ws, id), 'DDD')), ['aaa', 'bbb', 'ccc', 'ddd'].map((id) => symbolOf(ws, id)).join());
check('and the cells repaint on the new data', cellEls(ws).every((c) => painted(dataCanvas(c))));
ws.sync('symbol', false);
ws.cell('ddd').setSymbol('AAA');
await settle();
check('turning it off again isolates the cell', sameSymbol(symbolOf(ws, 'ddd'), 'AAA') && sameSymbol(symbolOf(ws, 'aaa'), 'DDD'));
// restore the four symbols for the rest
for (const [id, s] of [['aaa', 'AAA'], ['bbb', 'BBB'], ['ccc', 'CCC'], ['ddd', 'DDD']]) ws.cell(id).setSymbol(s);
await settle();

// ── crosshair sync ──

ws.sync('crosshair', true);
await settle();
for (const c of cellEls(ws)) cursorCanvas(c).ctx.ops = [];
const a = cellEl(ws, 'aaa');
pointer(dataCanvas(a), 'pointermove', a.rect.left + 200, a.rect.top + 120);
pump();
const followers = cellEls(ws).filter((c) => c !== a);
check('the crosshair moves with the pointer in the cell under it', painted(cursorCanvas(a)));
check('with the crosshair link on, the other cells show it too', followers.every((c) => painted(cursorCanvas(c))), followers.map((c) => painted(cursorCanvas(c))).join());
ws.sync('crosshair', false);
pointer(dataCanvas(a), 'pointerleave', 0, 0);
await settle();
for (const c of cellEls(ws)) cursorCanvas(c).ctx.ops = [];
pointer(dataCanvas(a), 'pointermove', a.rect.left + 220, a.rect.top + 130);
pump();
check('with it off, the other cells stay clean', followers.every((c) => !painted(cursorCanvas(c))));
pointer(dataCanvas(a), 'pointerleave', 0, 0);

// ── splitters ──

const widthBefore = dataCanvas(cellEl(ws, 'aaa')).width;
const widthRightBefore = dataCanvas(cellEl(ws, 'bbb')).width;
const strip = colStrip();
const down = pointer(strip, 'pointerdown', 500, 300);
check('a press on a strip starts a drag and is not left to the page', down.defaultPrevented === true);
for (let i = 1; i <= 5; i += 1) pointer(strip, 'pointermove', 500 + i * 40, 300);
pointer(strip, 'pointerup', 700, 300);
await settle();
const cols = tracksOf(grid.style.getPropertyValue('grid-template-columns'));
check('dragging a column strip trades weight between its two tracks', cols.length === 2 && cols[0] > 1.3 && cols[1] < 0.7 && Math.abs(cols[0] + cols[1] - 2) < 1e-9, grid.style.getPropertyValue('grid-template-columns'));
check('the cells resize: the left grows and the right shrinks', dataCanvas(cellEl(ws, 'aaa')).width > widthBefore + 100 && dataCanvas(cellEl(ws, 'bbb')).width < widthRightBefore - 100, `${widthBefore} -> ${dataCanvas(cellEl(ws, 'aaa')).width}`);
check('the bottom cells follow their column', dataCanvas(cellEl(ws, 'ccc')).width === dataCanvas(cellEl(ws, 'aaa')).width);
check('the strip followed the seam', Number.parseFloat(colStrip().style.getPropertyValue('left')) > W / 2 + 100, colStrip().style.getPropertyValue('left'));
check('the drag ended: a move after release changes nothing', (() => {
    const before = grid.style.getPropertyValue('grid-template-columns');
    pointer(strip, 'pointermove', 100, 300);
    return grid.style.getPropertyValue('grid-template-columns') === before;
})());
const reset = colStrip().dispatch('dblclick', {});
await settle();
check('a double click evens the axis out again', grid.style.getPropertyValue('grid-template-columns') === '1fr 1fr' && dataCanvas(cellEl(ws, 'aaa')).width === widthBefore, grid.style.getPropertyValue('grid-template-columns'));
check('rows drag too', (() => {
    const rowStrip = strips().find((s) => s.getAttribute('data-axis') === 'rows');
    pointer(rowStrip, 'pointerdown', 300, 300);
    pointer(rowStrip, 'pointermove', 300, 360);
    pointer(rowStrip, 'pointerup', 300, 360);
    const rows = tracksOf(grid.style.getPropertyValue('grid-template-rows'));
    strips().find((s) => s.getAttribute('data-axis') === 'rows').dispatch('dblclick', {});
    return rows[0] > rows[1] && grid.style.getPropertyValue('grid-template-rows') === '1fr 1fr';
})());
await settle();

// ── maximize ──

const maxEvents = [];
ws.on('cell:maximized', (e) => maxEvents.push(e.id));
ws.maximizeCell('bbb');
await settle();
const bbb = cellEl(ws, 'bbb');
check('maximize spans every cell over the grid, the maximized one on top', cellEls(ws).every((c) => c.style.getPropertyValue('grid-area') === '1 / 1 / -1 / -1') && bbb.style.getPropertyValue('z-index') === '5' && ws.maximizedCell() === 'bbb');
check('the other cells are hidden (and stay alive)', cellEls(ws).filter((c) => c !== bbb).every((c) => c.style.getPropertyValue('visibility') === 'hidden') && bbb.style.getPropertyValue('visibility') === '' && cellEls(ws).length === 4);
check('the grid is marked maximized and the maximized cell is active', grid.getAttribute('data-maximized') === '1' && ws.activeCell() === 'bbb' && maxEvents.at(-1) === 'bbb');
check('the maximized cell fills the grid', dataCanvas(bbb).width === W && dataCanvas(bbb).height === H, `${dataCanvas(bbb).width}x${dataCanvas(bbb).height}`);
const shotMax = ws.screenshot();
check('a maximized layout screenshot is the one cell, encoded as PNG', typeof shotMax === 'string' && shotMax.startsWith('data:image/png') && shotMax.includes(`size=${W}x${H}`), shotMax);
ws.maximizeCell(null);
await settle();
check('restoring brings the grid back', cellEls(ws).every((c) => c.style.getPropertyValue('visibility') === '' && c.style.getPropertyValue('grid-area') === '') && !grid.hasAttribute('data-maximized') && maxEvents.at(-1) === null);

// ── screenshot ──

const shot = ws.screenshot();
check('a multi-cell screenshot composites every cell on a grid-sized canvas', typeof shot === 'string' && shot.includes(`size=${W}x${H}`), shot);

// ── swap ──

ws.swapCells('aaa', 'bbb');
await settle();
check('swapping trades slots: the DOM order follows', cellEls(ws).map((c) => c.getAttribute('data-cell-id')).join() === 'bbb,aaa,ccc,ddd', cellEls(ws).map((c) => c.getAttribute('data-cell-id')).join());
ws.swapCells('aaa', 'bbb');
await settle();

// ── layout switching ──

const events = [];
const layoutOff = ws.on('layout:changed', (e) => events.push(`layout:${e.layout}`));
const destroyedOff = ws.on('cell:destroyed', (e) => events.push(`destroyed:${e.id}`));
const createdOff = ws.on('cell:created', (e) => events.push(`created:${e.id}`));
const paintedBefore = cellEl(ws, 'aaa');
check('setLayout with an unknown id is an error', (() => { try { ws.setLayout('zzz'); return false; } catch (e) { return String(e.message).includes('unknown workspace layout'); } })());
ws.setLayout('2h');
await settle();
check('a two-cell layout drops two cells and removes their DOM', cellEls(ws).length === 2 && ws.cellIds().join() === 'aaa,bbb' && !cellEl(ws, 'ccc') && !cellEl(ws, 'ddd') && events.filter((e) => e.startsWith('destroyed:')).length === 2, events.join());
check('the surviving cells keep their live charts', cellEl(ws, 'aaa') === paintedBefore && painted(dataCanvas(cellEl(ws, 'aaa'))));
check('the template follows the layout', ws.layout() === '2h' && grid.getAttribute('data-multi') === '1' && tracksOf(grid.style.getPropertyValue('grid-template-columns')).length + tracksOf(grid.style.getPropertyValue('grid-template-rows')).length === 3, `${grid.style.getPropertyValue('grid-template-columns')} / ${grid.style.getPropertyValue('grid-template-rows')}`);
check('the strips follow (one seam)', strips().length === 1, `${strips().length}`);
check('the mark moved to the bottom-left cell of the new layout', (() => {
    const m = root.all((n) => n.className === 'vela-attribution' && n.style.getPropertyValue('display') !== 'none');
    return m.length === 1 && cellEls(ws).includes(m[0].parentNode);
})());
ws.setLayout('4');
await settle();
check('back to four cells re-creates the dropped ones', cellEls(ws).length === 4 && cellEls(ws).every((c) => painted(dataCanvas(c))) && events.filter((e) => e.startsWith('created:')).length === 2 && events.includes('layout:4'), events.join());
check('and restores their markets from the pool', sameSymbol(symbolOf(ws, 'ccc'), 'CCC') && sameSymbol(symbolOf(ws, 'ddd'), 'DDD'));
layoutOff();
destroyedOff();
createdOff();
check('on() returns a function that removes the handler', (() => { const n = events.length; ws.setLayout('2h'); ws.setLayout('4'); return events.length === n; })());
await settle();

// ── state: indicators and drawings, then a round trip ──

ws.setActiveCell('aaa');
const emaId = ws.cell('aaa').addIndicator('ema', { length: 21 });
ws.cell('ccc').addIndicator('rsi', undefined);
await settle();
check('addIndicator returns the id and the chart lists it', typeof emaId === 'string' && ws.cell('aaa').indicators().includes(emaId) && ws.cell('ccc').indicators().length === 2, emaId);
ws.cell('aaa').armDrawingTool('trendline');
const aData = dataCanvas(cellEl(ws, 'aaa'));
const aRect = cellEl(ws, 'aaa').rect;
pointer(aData, 'pointerdown', aRect.left + 150, aRect.top + 120);
pointer(aData, 'pointerup', aRect.left + 150, aRect.top + 120);
await settle();
pointer(aData, 'pointerdown', aRect.left + 330, aRect.top + 160);
pointer(aData, 'pointerup', aRect.left + 330, aRect.top + 160);
await settle();
check('two clicks draw a trend line in the active cell', ws.cell('aaa').drawingCount() === 1 && ws.cell('bbb').drawingCount() === 0, `${ws.cell('aaa').drawingCount()}`);
ws.setLayout('2v');
await settle();
const layoutBefore = ws.layout();
const stateDoc = ws.getState();
check('getState is a plain document with the layout, the cells and the sync links', stateDoc.version === 1 && stateDoc.layout === layoutBefore && stateDoc.activeCellId === 'aaa' && stateDoc.charts.length >= 2 && typeof stateDoc === 'object', JSON.stringify(Object.keys(stateDoc)));
check('the document carries the indicator and the drawing', JSON.stringify(stateDoc).includes('"ema"') && JSON.stringify(stateDoc).includes('trendline'));
const events2 = [];
ws.on('state:changed', () => events2.push(1));
const toastsBefore = grid.all((n) => n.className === 'vela-toast')[0];
ws.toast('Merhaba', 'success', 0);
check('a toast shows on the status pill', toastsBefore.getAttribute('data-open') === '1' && toastsBefore.getAttribute('data-type') === 'success' && toastsBefore.textContent === 'Merhaba');
ws.toast('Hata', 'error', 20);
check('a newer toast replaces it and is typed', toastsBefore.getAttribute('data-type') === 'error' && toastsBefore.textContent === 'Hata');
await new Promise((resolve) => setTimeout(resolve, 60));
check('the toast hides itself after its delay', !toastsBefore.hasAttribute('data-open'));
check('an unknown toast kind is an error', (() => { try { ws.toast('x', 'loud'); return false; } catch (e) { return String(e.message).includes('unknown toast kind'); } })());

// persistence: the same key survives a re-create
ws.saveNow();
const saved = storage.get('vela-smoke-workspace');
check('the state is persisted to localStorage under the given key', typeof saved === 'string' && JSON.parse(saved).layout === layoutBefore && JSON.parse(saved).version === 1);
ws.destroy();
await settle();
check('destroy removes every DOM node and listener it added', host.children.length === 0 && totalListeners(host) === listenersBefore && observers.filter((o) => o.connected).length === 0, `children ${host.children.length}, listeners ${totalListeners(host)} vs ${listenersBefore}, observers ${observers.filter((o) => o.connected).length}`);
check('destroy is idempotent and a dead workspace refuses calls', (() => { ws.destroy(); try { ws.layout(); return false; } catch (e) { return String(e.message).includes('destroyed'); } })());

current = null;
const ws2 = new wasm.VelaWorkspace(host, { layout: '4', bars: seedBars(), persist: 'vela-smoke-workspace' });
current = ws2;
await ws2.ready();
await settle();
check('a new workspace on the same key restores the layout, the symbols and the indicators', ws2.layout() === layoutBefore && sameSymbol(symbolOf(ws2, 'aaa'), 'AAA') && JSON.stringify(ws2.getState()).includes('"ema"'), `${ws2.layout()} / ${ws2.cellIds().join()}`);
check('and the drawings', ws2.cell('aaa').drawingCount() === 1);
check('and every restored cell paints', cellEls(ws2).every((c) => painted(dataCanvas(c))));

// getState -> destroy -> new -> applyState
const exported = ws2.getState();
ws2.setLayout('4');
await settle();
ws2.applyState(exported);
await settle();
check('applyState restores the layout it was given', ws2.layout() === layoutBefore && ws2.cellIds().length === cellEls(ws2).length);
ws2.destroy();
await settle();
storage.clear();
const ws3 = new wasm.VelaWorkspace(host, { layout: '4', bars: seedBars() });
current = ws3;
await ws3.ready();
await settle();
check('without persist nothing is read or written', ws3.layout() === '4' && storage.size === 0);
ws3.applyState(exported);
await settle();
check('applyState on a fresh workspace restores layout, symbols, indicators and drawings', ws3.layout() === layoutBefore && sameSymbol(symbolOf(ws3, 'aaa'), 'AAA') && JSON.stringify(ws3.getState()).includes('"ema"') && ws3.cell('aaa').drawingCount() === 1, `${ws3.layout()}, ${ws3.cell('aaa').drawingCount()} drawings`);
check('applyState ignores junk', (() => { try { ws3.applyState({ nonsense: true }); ws3.applyState(null); return ws3.layout() === layoutBefore; } catch (e) { return false; } })());

// ticks reach the charts through the in-memory provider
const lastBar = bars(100, 50).at(-1);
const delivered = ws3.tick('AAA', '60', { ...lastBar, close: lastBar.close + 2, high: lastBar.high + 3 });
check('a tick is delivered to the chart that shows the symbol', delivered >= 1, `${delivered}`);
check('a bad tick is an error', (() => { try { ws3.tick('AAA', '60', { time: 1 }); return false; } catch (e) { return String(e.message).includes('bar.open'); } })());

// theme, locale, timezone
ws3.setTheme('light');
await settle();
const root3 = rootOf(host);
check('the theme re-writes the root tokens and the mark ink', root3.style.getPropertyValue('--vela-bg') === '#ffffff' && root3.all((n) => n.className === 'vela-attribution').every((m) => m.style.getPropertyValue('color') === '#000000'), root3.style.getPropertyValue('--vela-bg'));
check('and every cell paints the light background', cellEls(ws3).every((c) => c.children[0].style.getPropertyValue('background') === '#ffffff'));
check('an unknown theme is an error', (() => { try { ws3.setTheme('neon'); return false; } catch (e) { return String(e.message).includes('unknown theme'); } })());
const localeEvents = [];
ws3.on('locale:changed', (e) => localeEvents.push(e));
ws3.setLocale('tr');
await settle();
check('setLocale tells the workspace and sets lang on the root', localeEvents.length === 1 && localeEvents[0].locale === 'tr' && root3.getAttribute('lang') === 'tr' && !root3.hasAttribute('dir'), JSON.stringify(localeEvents));
check('without a host direction the root carries lang only (dir is stamped only when the host chose one)', !root3.hasAttribute('dir'));
ws3.setLocale({ locale: 'ar', direction: 'auto', hourCycle: 'h12', weekStart: 6 });
await settle();
check('setLocale takes an options object: direction auto follows the script, dir is stamped once chosen', root3.getAttribute('lang') === 'ar' && root3.getAttribute('dir') === 'rtl', `${root3.getAttribute('lang')} ${root3.getAttribute('dir')}`);
ws3.setLocale({ direction: 'ltr' });
check('absent fields keep their value', root3.getAttribute('lang') === 'ar' && root3.getAttribute('dir') === 'ltr');
check('a bad direction or option value is an error', (() => { try { ws3.setLocale({ direction: 'up' }); return false; } catch (e) { return String(e.message).includes('unknown direction'); } })() && (() => { try { ws3.setLocale(5); return false; } catch (e) { return String(e.message).includes('locale tag'); } })());
ws3.setLocale('tr');
ws3.setTimezone('Europe/Istanbul');
check('setTimezone is read back', ws3.timezone() === 'Europe/Istanbul');
ws3.setTheme('dark');
await settle();

// layout mode follows the root width
root3.rect = { left: 0, top: 0, width: 500, height: H };
host.rect = { left: 0, top: 0, width: 500, height: H };
relayout();
await settle();
check('a narrow root switches to the mobile layout mode', root3.getAttribute('data-layout') === 'mobile' && ws3.layoutMode() === 'mobile');
host.rect = { left: 0, top: 0, width: W, height: H };
root3.rect = { left: 0, top: 0, width: W, height: H };
relayout();
await settle();
check('and back to desktop when it widens', root3.getAttribute('data-layout') === 'desktop' && ws3.layoutMode() === 'desktop');

// unload flush
ws3.destroy();
await settle();

// persistence: pagehide and visibilitychange flush the pending state
storage.clear();
current = null;
const ws4 = new wasm.VelaWorkspace(host, { layout: '4', bars: seedBars(), persist: true });
current = ws4;
await ws4.ready();
await settle();
ws4.setLayout('2h');
check('nothing is written inside the debounce window', !storage.has('vela-workspace'));
doc.defaultView.dispatch('pagehide', {});
const flushed = storage.get('vela-workspace');
check('pagehide flushes the pending state (persist: true is the vela-workspace key)', typeof flushed === 'string' && JSON.parse(flushed).layout === '2h', String(flushed).slice(0, 40));
storage.clear();
ws4.setLayout('4');
doc.visibilityState = 'hidden';
doc.dispatch('visibilitychange', {});
check('a hidden page flushes too', typeof storage.get('vela-workspace') === 'string' && JSON.parse(storage.get('vela-workspace')).layout === '4');
doc.visibilityState = 'visible';
ws4.destroy();
await settle();

// a blocked localStorage is never fatal
storageControl.failing = true;
current = null;
const ws5 = new wasm.VelaWorkspace(host, { layout: '2v', bars: seedBars(), persist: 'k' });
current = ws5;
await ws5.ready();
await settle();
ws5.setLayout('4');
ws5.saveNow();
check('a blocked localStorage does not break the workspace', ws5.layout() === '4' && cellEls(ws5).length === 4);
storageControl.failing = false;
ws5.destroy();
await settle();

// ── the widget: one cell, same root ──

current = null;
const widgetHost = container(W, H);
const widget = new wasm.VelaWidget(widgetHost, { bars: seedBars(), symbol: 'BBB', timeframe: '60', persist: true });
current = { grid: () => widget.root().all((n) => n.className === 'vela-ws-grid')[0] };
await widget.ready();
await settle();
const wroot = rootOf(widgetHost);
check('the widget root has the same named regions', widget.regions().length === 9 && widget.regions().every((r) => widget.region(r) !== undefined) && wroot.children.length === 9);
const wcells = cellsOf(widget.workspace().grid());
check('the widget is one cell on the single layout, no active ring', wcells.length === 1 && widget.workspace().layout() === '1' && widget.workspace().grid().getAttribute('data-multi') === null && painted(dataCanvas(wcells[0])));
check('the widget cell fills the grid and shows its symbol', dataCanvas(wcells[0]).width === W && sameSymbol(widget.symbol(), 'BBB') && widget.timeframe() === '60');
widget.setSymbol('CCC');
await settle();
check('the widget switches symbol', sameSymbol(widget.symbol(), 'CCC'));
widget.workspace().setLayout('4');
check('a single-chart widget ignores setLayout', widget.workspace().layout() === '1' && cellsOf(widget.workspace().grid()).length === 1);
const widgetShot = widget.screenshot();
check('the widget screenshot goes through the cell pile', typeof widgetShot === 'string' && widgetShot.includes(`size=${W}x${H}`), widgetShot);
const widgetState = widget.getState();
widget.destroy();
await settle();
check('widget persistence uses the vela-widget key', widgetState.layout === '1' && storage.has('vela-widget') && JSON.parse(storage.get('vela-widget')).layout === '1');
check('destroying the widget removes its DOM', widgetHost.children.length === 0);

/// ── registerTheme: the page-wide registry, usable by setTheme on every entry class ──

const MIDNIGHT = { background: '#010203', textColor: '#cccccc', gridColor: '#111111', borderColor: '#222222', upColor: '#00ff00', downColor: '#ff0000', fontFamily: 'sans-serif' };
const offMidnight = wasm.registerTheme({ id: 'midnight', label: 'Midnight', theme: MIDNIGHT });
check('registeredThemes lists a registered theme with its label and data', wasm.registeredThemes().some((t) => t.id === 'midnight' && t.label === 'Midnight' && t.theme.background === '#010203'));
check('a built-in id cannot be registered', ['dark', 'light'].every((id) => { try { wasm.registerTheme({ id, label: id, theme: MIDNIGHT }); return false; } catch (e) { return String(e.message).includes('built-in'); } }));
check('a registration without an id or a theme object is an error', (() => { try { wasm.registerTheme({ label: 'x' }); return false; } catch (e) { return String(e.message).includes('registerTheme expects'); } })());
current = null;
const hostT = container(W, H);
const wsT = new wasm.VelaWorkspace(hostT, { layout: '2h', bars: seedBars(), theme: 'midnight' });
current = wsT;
await wsT.ready();
await settle();
const rootT = rootOf(hostT);
check('a workspace boots on a registered theme id (tokens from its colors)', rootT.style.getPropertyValue('--vela-bg') === '#010203', rootT.style.getPropertyValue('--vela-bg'));
wsT.setTheme('light');
await settle();
check('setTheme moves off it', rootT.style.getPropertyValue('--vela-bg') === '#ffffff');
wsT.setTheme('midnight');
await settle();
check('setTheme(id) takes a registered theme on a live workspace', rootT.style.getPropertyValue('--vela-bg') === '#010203' && cellEls(wsT).every((c) => c.children[0].style.getPropertyValue('background') === '#010203'), rootT.style.getPropertyValue('--vela-bg'));
const lateOff = wasm.registerTheme({ id: 'late', label: 'Late', theme: { ...MIDNIGHT, background: '#0a0b0c' } });
wsT.setTheme('late');
await settle();
check('a theme registered after the workspace was built reaches it', rootT.style.getPropertyValue('--vela-bg') === '#0a0b0c');
const chartHost = container(400, 300);
const lone = new wasm.VelaChart(chartHost, 'late');
check('VelaChart takes a registered theme in its constructor and in setTheme', (() => { lone.setTheme('midnight'); lone.setTheme('late'); return true; })());
lone.destroy();
const appHost = container(400, 300);
const lapp = new wasm.VelaApp(appHost, 'midnight', 'DEMO', '1h', bars(5, 50, 80));
check('VelaApp takes a registered theme and setTheme', (() => { lapp.setTheme('late'); lapp.setTheme('dark'); return true; })());
lapp.destroy();
const widgetHostT = container(W, H);
const widgetT = new wasm.VelaWidget(widgetHostT, { bars: seedBars(), symbol: 'AAA', theme: 'late' });
await widgetT.ready();
await settle();
widgetT.setTheme('midnight');
check('VelaWidget.setTheme takes a registered theme', rootOf(widgetHostT).style.getPropertyValue('--vela-bg') === '#010203', rootOf(widgetHostT).style.getPropertyValue('--vela-bg'));
widgetT.destroy();
lateOff();
check('the remover unregisters (a second call is harmless)', (() => { lateOff(); try { wsT.setTheme('late'); return false; } catch (e) { return String(e.message).includes('unknown theme'); } })());
offMidnight();
check('and an unregistered id is unknown again', !wasm.registeredThemes().some((t) => t.id === 'midnight' || t.id === 'late') && (() => { try { wsT.setTheme('midnight'); return false; } catch (e) { return String(e.message).includes('unknown theme'); } })());
wsT.destroy();
await settle();

// ── the WebGL budget: backend, maxWebglCells, a layout change that crosses it rebuilds ──

options.webgl = true;
current = null;
const backendsOf = (w) => w.cellIds().map((id) => w.cell(id).backend());
const hostB = container(W, H);
const wsB = new wasm.VelaWorkspace(hostB, { layout: '2h', maxWebglCells: 2, bars: seedBars(), cells: Object.fromEntries(['aaa', 'bbb', 'ccc', 'ddd'].map((id, i) => [id, { symbol: SYMBOLS[i], timeframe: '60' }])) });
current = wsB;
await wsB.ready();
await settle();
check('within the budget every cell takes WebGL2 (auto)', backendsOf(wsB).join() === 'webgl2,webgl2', backendsOf(wsB).join());
const aaaBefore = cellEl(wsB, 'aaa');
wsB.setLayout('4');
await settle();
check('a layout past maxWebglCells rebuilds every cell on canvas2d', backendsOf(wsB).join() === 'canvas2d,canvas2d,canvas2d,canvas2d' && cellEl(wsB, 'aaa') !== aaaBefore, backendsOf(wsB).join());
wsB.setLayout('2h');
await settle();
check('back inside the budget they are rebuilt on WebGL2 again', backendsOf(wsB).join() === 'webgl2,webgl2', backendsOf(wsB).join());
wsB.destroy();
await settle();
const hostB2 = container(W, H);
const wsB2 = new wasm.VelaWorkspace(hostB2, { layout: '4', maxWebglCells: 0, backend: 'webgl2', bars: seedBars() });
current = wsB2;
await wsB2.ready();
await settle();
check('an explicit backend wins over the budget policy', backendsOf(wsB2).every((b) => b === 'webgl2'), backendsOf(wsB2).join());
wsB2.destroy();
await settle();
const hostB3 = container(W, H);
const wsB3 = new wasm.VelaWorkspace(hostB3, { layout: '2h', backend: 'canvas2d', bars: seedBars() });
current = wsB3;
await wsB3.ready();
await settle();
check('backend canvas2d forces 2D even where WebGL2 exists', backendsOf(wsB3).every((b) => b === 'canvas2d'), backendsOf(wsB3).join());
wsB3.destroy();
await settle();
check('an unknown backend, a negative budget or a negative glow is an error', (() => {
    const bad = (o) => { try { new wasm.VelaWorkspace(container(W, H), { bars: seedBars(), ...o }); return false; } catch (e) { return String(e.message).includes('options.') || String(e.message).includes('backend'); } };
    return bad({ backend: 'vulkan' }) && bad({ maxWebglCells: -1 }) && bad({ glow: -1 });
})());
options.webgl = false;
current = null;

// ── a CSS selector as the container ──

const selHost = container(W, H);
selHost.id = 'sel-host';
doc.querySelector ??= (sel) => (sel === '#sel-host' ? selHost : null);
const selWs = new wasm.VelaWorkspace('#sel-host', { layout: false, bars: seedBars() });
check('the container may be a selector string', selHost.children.length === 1);
selWs.destroy();
check('an unknown selector is an error', (() => { try { new wasm.VelaWorkspace('#nope', {}); return false; } catch (e) { return String(e.message).includes('container not found'); } })());
await settle();

// providers: asking for an unknown one is an error, none is contacted
check('an unknown provider name is an error', (() => { try { new wasm.VelaWorkspace(container(W, H), { providers: ['nope'] }); return false; } catch (e) { return String(e.message).includes('unknown provider'); } })());

check('every timer and frame is idle at the end', frames.size === 0);
tick(1);

if (state.failures > 0) {
    console.error(`${state.failures} check(s) failed`);
    process.exit(1);
}
console.log('smoke_workspace: all checks passed');
// The platform keeps no handles open; leave explicitly so a pending debounce timer cannot hold Node.
process.exit(0);
