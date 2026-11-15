// Smoke test of the SIDE of the widget shell (vela-web `shell_side`): the sidebar, the docked
// side panels (object tree, data window, contributed panels), the chart context menu, the
// default key bindings, the shortcuts panel and the mobile surfaces, in Node against the
// minimal fake DOM.
//
// The fake DOM has no layout engine: this script plays one (the grid cells, and the rows of the
// object tree when a drag needs geometry). Nothing here runs a browser: real layout, pointer
// capture, painting, focus behavior and CSS (the narrow-width overlay, the mobile visibility
// flips) are not exercised. No exchange endpoint is ever called: the in-memory provider is the
// only data source.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_shell_side.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, check, clock, container, doc, frames, loadPackage, observers, options, painted, pump, state, storage } from './fake_dom.mjs';

options.captureAncestors = true;
const wasm = await loadPackage('smoke_shell_side.mjs');

const W = 1000;
const H = 600;
const GAP = 2;
const SYMBOLS = ['AAA', 'BBB'];

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
        const span = [at, (content * w) / sum];
        at += (content * w) / sum + GAP;
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
        if (area.includes('/')) cell.rect = { left: 0, top: 0, width, height };
        else {
            const [x, w] = cols[i % cols.length];
            const [y, h] = rows[Math.floor(i / cols.length)] ?? rows.at(-1);
            cell.rect = { left: x, top: y, width: w, height: h };
        }
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
    const plot = cell.children[0].children[0];
    return { plot, canvases: plot.children.filter((c) => c instanceof FakeCanvas) };
};
const dataCanvas = (cell) => pileOf(cell).canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
const pointer = (target, type, x, y, extra = {}) =>
    target.dispatch(type, { clientX: x, clientY: y, pointerId: 3, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock, ...extra });
const cls = (n, c) => String(n.className).split(' ').includes(c);
const byClass = (node, c) => node.all((n) => cls(n, c));
const one = (node, c) => byClass(node, c)[0];
const totalListeners = (node) => node.listeners.length + node.children.reduce((n, c) => n + totalListeners(c), 0);
const key = (target, k, extra = {}) =>
    target.dispatch('keydown', { key: k, code: '', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });
const click = (el, extra = {}) => el.dispatch('click', { button: 0, ...extra });
// The top part of the shell listens to the intents and opens its own surfaces (symbol search,
// pickers, drawers): close whatever dialog it opened, so the checks stay independent.
const dismiss = async (rootEl) => {
    for (let i = 0; i < 4; i += 1) {
        const open = rootEl.all((n) => n.getAttribute('role') === 'dialog' && !cls(n, 'vela-sc') && !cls(n, 'vela-sheet'));
        if (open.length === 0) return;
        open[0].dispatch('keydown', { key: 'Escape', code: 'Escape', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false });
        await new Promise((resolve) => setTimeout(resolve, 30)); // an exit animation finishes on a timer
    }
};
const labels = (node, c) => byClass(node, c).map((n) => n.textContent);

// ── contributions: registered BEFORE the widget (a widget reads the registries on mount) ──

const mounts = { rs: 0, mx: 0, ov: 0 };
const opened = [];
const destroyed = [];
const charts = [];
wasm.registerSidePanel({
    id: 'rs', title: 'Resizable', icon: 'pen', order: 100, width: 280, resizable: true, minWidth: 200, maxWidth: 400,
    mount(body) {
        mounts.rs += 1;
        body.textContent = 'resizable body';
        return { onOpen: () => opened.push('rs'), onChart: () => charts.push('rs'), destroy: () => destroyed.push('rs') };
    },
});
wasm.registerSidePanel({
    id: 'mx', title: 'Maximizable', icon: 'pen', order: 101, maximizable: true,
    mount(body) {
        mounts.mx += 1;
        body.textContent = 'maximizable body';
    },
});
wasm.registerSidePanel({
    id: 'ov', title: 'Pinnable', icon: 'pen', order: 102, overlay: true, width: 300,
    mount(body) {
        mounts.ov += 1;
        body.textContent = 'overlay body';
    },
});
const hello = { ran: [], active: false };
wasm.registerWidgetAction({ id: 'hello', target: 'sidebar', label: 'Hello', icon: 'star', run: (ctx) => hello.ran.push(ctx.symbol), active: () => hello.active });
const rows = { pointers: [] };
wasm.registerWidgetAction({ id: 'ctxrow', target: 'context:body', label: 'Contributed row', icon: 'star', order: 100, run: (ctx) => rows.pointers.push(ctx.pointer) });
wasm.registerWidgetAction({ id: 'axisrow', target: 'context:price-axis', label: 'Axis row', icon: 'star', run: () => {} });
wasm.registerWidgetAction({ id: 'ctxfirst', target: 'context:body', label: 'Copy price', icon: 'clone', order: -100, run: () => {} });

// ── the widget ──

const host = container(W, H);
const listenersBefore = totalListeners(host);
const docListeners = () => doc.listeners.length + doc.defaultView.listeners.length;
const docBefore = docListeners();
const ws = new wasm.VelaWorkspace(host, {
    layout: '2h',
    bars: seedBars(),
    theme: 'dark',
    persist: 'vela-smoke-side',
    sidebar: true,
    cells: { aaa: { symbol: 'AAA', timeframe: '60' }, bbb: { symbol: 'BBB', timeframe: '60' } },
});
current = ws;
await ws.ready();
await settle();
const root = rootOf(host);
const grid = ws.grid();
const main = ws.region('main');
const panelEl = (id) => main.children.find((c) => cls(c, 'vela-panel') && c.getAttribute('data-panel-id') === id);
const shown = (el) => el && !el.hidden;
const intents = [];
root.addEventListener('vela:shell-intent', (e) => intents.push(e.detail));

// ── the sidebar ──

const edge = ws.region('edge');
const nav = edge.children[0];
check('the sidebar mounts in the edge region as a labelled nav', nav && nav.tagName === 'NAV' && cls(nav, 'vela-sidebar') && nav.getAttribute('aria-label') === 'Sidebar' && root.hasAttribute('data-vela-sidebar'));
const btns = () => byClass(nav, 'vela-sidebar-btn');
const btn = (id) => btns().find((b) => b.getAttribute('data-id') === id);
const panelIds = () => btns().filter((b) => b.getAttribute('data-kind') === 'panel').map((b) => b.getAttribute('data-id'));
check('the panel toggles come in the original order (the default layout: contributed panels on top, the inspection panels below, then the actions)', panelIds().join() === 'rs,mx,ov,objects,dataWindow', panelIds().join());
check('every entry has an accessible name', btns().every((b) => b.getAttribute('aria-label')) && btn('dataWindow').getAttribute('aria-label') === 'Data window' && btn('objects').getAttribute('aria-label') === 'Object tree' && btn('rs').getAttribute('aria-label') === 'Resizable');
check('the sections: contributed panels on top; below, the inspection panels, a separator, the contributed action', byClass(nav, 'vela-sidebar-section').length === 2 && byClass(nav.children[0], 'vela-sidebar-btn').map((b) => b.getAttribute('data-id')).join() === 'rs,mx,ov' && byClass(nav.children[1], 'vela-sidebar-btn').map((b) => b.getAttribute('data-id')).join() === 'objects,dataWindow,hello' && byClass(nav.children[1], 'vela-sidebar-sep').length === 1);
check('the action button carries the plugin anchor hook and its label', btn('hello').getAttribute('data-action-id') === 'hello' && btn('hello').getAttribute('aria-label') === 'Hello');
check('every toggle starts released (aria-pressed) and every panel hidden', btns().filter((b) => b.getAttribute('data-kind') === 'panel').every((b) => b.getAttribute('aria-pressed') === 'false') && ['dataWindow', 'objects', 'rs', 'mx', 'ov'].every((id) => panelEl(id) && panelEl(id).hidden));
check('contributed panels mounted once into their bodies', mounts.rs === 1 && mounts.mx === 1 && mounts.ov === 1 && one(panelEl('rs'), 'vela-panel-body').textContent === 'resizable body');

// the single-open rule, persisted state
click(btn('dataWindow'));
check('a toggle opens its panel and presses its button', shown(panelEl('dataWindow')) && btn('dataWindow').getAttribute('aria-pressed') === 'true');
check('the open panel is persisted with the workspace document', ws.getState().panels?.open === 'dataWindow', JSON.stringify(ws.getState().panels));
click(btn('objects'));
check('opening another panel closes the first (single-open rule)', shown(panelEl('objects')) && panelEl('dataWindow').hidden && btn('dataWindow').getAttribute('aria-pressed') === 'false' && btn('objects').getAttribute('aria-pressed') === 'true' && btns().filter((b) => b.getAttribute('aria-pressed') === 'true').length === 1);
check('and the document follows', ws.getState().panels?.open === 'objects');
click(btn('objects'));
check('a second press closes it and the document forgets it', panelEl('objects').hidden && btn('objects').getAttribute('aria-pressed') === 'false' && !ws.getState().panels?.open, JSON.stringify(ws.getState().panels));
click(btn('hello'));
check('an action button runs its action with a live context', hello.ran.length === 1 && hello.ran[0].endsWith('AAA'), hello.ran.join());
hello.active = true;
click(btn('hello'));
check('a sidebar action re-reads its pressed look after it ran', btn('hello').getAttribute('aria-pressed') === 'true');

// ── a docked panel: header, close, the contributed handle ──

click(btn('rs'));
check('a contributed panel opens and its handle hears it', shown(panelEl('rs')) && opened.includes('rs'));
const rsPanel = panelEl('rs');
check('the panel has a titled header with a labelled close button', one(rsPanel, 'vela-panel-title').textContent === 'Resizable' && one(rsPanel, 'vela-panel-close').getAttribute('aria-label') === 'Close');
check('the handle was bound to the active chart', charts.length >= 1);
click(one(rsPanel, 'vela-panel-close'));
check('the header close closes the panel and releases its toggle', rsPanel.hidden && btn('rs').getAttribute('aria-pressed') === 'false');

// resize by drag within the clamp
click(btn('rs'));
const widthOf = (el) => el.style.getPropertyValue('--vela-panel-w');
const resizer = one(rsPanel, 'vela-panel-resizer');
check('a resizable panel starts at its declared width with a separator handle', widthOf(rsPanel) === '280px' && resizer.getAttribute('role') === 'separator');
pointer(resizer, 'pointerdown', 600, 300);
pointer(resizer, 'pointermove', 540, 300);
check('dragging the handle away from the edge widens the panel', widthOf(rsPanel) === '340px', widthOf(rsPanel));
pointer(resizer, 'pointermove', 100, 300);
check('the width stops at the clamp (max)', widthOf(rsPanel) === '400px', widthOf(rsPanel));
pointer(resizer, 'pointermove', 900, 300);
check('and at the other (min)', widthOf(rsPanel) === '200px', widthOf(rsPanel));
pointer(resizer, 'pointermove', 520, 300);
pointer(resizer, 'pointerup', 520, 300);
check('the settled width is persisted with the dock state', ws.getState().panels?.widths?.rs === 360, JSON.stringify(ws.getState().panels));
pointer(resizer, 'pointermove', 100, 300);
check('a move after release changes nothing', widthOf(rsPanel) === '360px');
resizer.dispatch('dblclick', {});
check('a double click returns to the declared width and reports it', widthOf(rsPanel) === '280px' && ws.getState().panels?.widths?.rs === 280);

// maximize
click(btn('mx'));
const mxPanel = panelEl('mx');
const mxBtn = one(mxPanel, 'vela-panel-max');
check('a maximizable panel has a labelled maximize button', mxBtn && mxBtn.getAttribute('aria-label') === 'Maximize' && mxBtn.getAttribute('aria-pressed') === 'false');
click(mxBtn);
check('maximize covers the shell and the button becomes Restore', mxPanel.getAttribute('data-maximized') === '1' && mxBtn.getAttribute('aria-label') === 'Restore' && mxBtn.getAttribute('aria-pressed') === 'true');
click(mxBtn);
check('restore puts it back', !mxPanel.hasAttribute('data-maximized') && mxBtn.getAttribute('aria-label') === 'Maximize');
click(mxBtn);
click(one(mxPanel, 'vela-panel-close'));
click(btn('mx'));
check('a closed panel reopens at its own size (maximizing is a moment)', shown(mxPanel) && !mxPanel.hasAttribute('data-maximized'));

// overlay and pin
click(btn('ov'));
const ovPanel = panelEl('ov');
const pin = one(ovPanel, 'vela-panel-pin');
check('a floating panel overlays the chart and carries a pin', ovPanel.getAttribute('data-overlay') === '1' && pin && pin.getAttribute('aria-pressed') === 'false');
click(pin);
check('pinning docks it as a column and the choice is persisted', !ovPanel.hasAttribute('data-overlay') && pin.getAttribute('aria-pressed') === 'true' && (ws.getState().panels?.pinned ?? []).includes('ov'), JSON.stringify(ws.getState().panels));
click(pin);
check('unpinning floats it again', ovPanel.getAttribute('data-overlay') === '1' && !(ws.getState().panels?.pinned ?? []).includes('ov'));
click(btn('ov'));

// ── data window ──

click(btn('dataWindow'));
const dw = panelEl('dataWindow');
check('the data window panel is titled and lists a readout', one(dw, 'vela-panel-title').textContent === 'Data window' && byClass(dw, 'vela-dw-row').length > 0 && labels(dw, 'vela-dw-label').includes('Close'), labels(dw, 'vela-dw-label').join());
const dwValue = (label) => {
    const row = byClass(dw, 'vela-dw-row').find((r) => one(r, 'vela-dw-label').textContent === label);
    return row ? one(row, 'vela-dw-value').textContent : null;
};
const aRect = cellEl(ws, 'aaa').rect;
const aData = dataCanvas(cellEl(ws, 'aaa'));
pointer(aData, 'pointermove', aRect.left + 100, aRect.top + 120);
pump();
const closeAt100 = dwValue('Close');
pointer(aData, 'pointermove', aRect.left + 300, aRect.top + 120);
pump();
const closeAt300 = dwValue('Close');
check('the data window shows values that follow the crosshair', closeAt100 && closeAt300 && closeAt100 !== closeAt300, `${closeAt100} / ${closeAt300}`);
check('the OHLC lines carry the direction color and the values are left-to-right isolates', byClass(dw, 'vela-dw-value').some((v) => v.style.getPropertyValue('color').startsWith('var(--vela-')) && byClass(dw, 'vela-dw-value').every((v) => v.getAttribute('dir') === 'ltr'));
pointer(aData, 'pointerleave', 0, 0);
click(btn('dataWindow'));

// the data window follows the active cell
ws.setActiveCell('bbb');
await settle();
click(btn('dataWindow'));
const bData = dataCanvas(cellEl(ws, 'bbb'));
const bRect = cellEl(ws, 'bbb').rect;
pointer(bData, 'pointermove', bRect.left + 100, bRect.top + 120);
pump();
const closeB = dwValue('Close');
check('the panel follows the active cell: its readout is the other chart\'s', closeB && closeB !== closeAt100, `${closeB} vs ${closeAt100}`);
pointer(bData, 'pointerleave', 0, 0);
click(btn('dataWindow'));
ws.setActiveCell('aaa');
await settle();

// ── the object tree ──

ws.cell('aaa').addIndicator('ema', { length: 21 });
await settle();
const draw = async (x1, y1, x2, y2) => {
    ws.cell('aaa').armDrawingTool('trendline');
    pointer(aData, 'pointerdown', aRect.left + x1, aRect.top + y1);
    pointer(aData, 'pointerup', aRect.left + x1, aRect.top + y1);
    await settle();
    pointer(aData, 'pointerdown', aRect.left + x2, aRect.top + y2);
    pointer(aData, 'pointerup', aRect.left + x2, aRect.top + y2);
    await settle();
};
await draw(100, 100, 300, 140);
await draw(120, 200, 320, 230);
check('two trend lines were drawn on the active cell', ws.cell('aaa').drawingCount() === 2 && ws.cell('bbb').drawingCount() === 0, `${ws.cell('aaa').drawingCount()}`);
click(btn('objects'));
const ot = panelEl('objects');
const body = one(ot, 'vela-panel-body');
const rowsOf = (kind) => byClass(body, 'vela-ot-row').filter((r) => r.getAttribute('data-row-kind') === kind);
const rowName = (r) => one(r, 'vela-ot-name')?.textContent;
check('the tree lists the price series, the indicator and both drawings of the active cell', rowsOf('price').length === 1 && rowsOf('indicator').some((r) => /EMA/i.test(rowName(r))) && rowsOf('drawing').length === 2, `${rowsOf('price').length}/${rowsOf('indicator').length}/${rowsOf('drawing').length}`);
check('the panes are blocks with a name and the main chart is first', byClass(body, 'vela-ot-pane').length >= 1 && byClass(body, 'vela-ot-pane')[0].getAttribute('data-kind') === 'price' && one(body, 'vela-ot-panename').textContent === 'Main chart');
check('drawing rows are named after their type and draggable', rowsOf('drawing').every((r) => /trend/i.test(rowName(r)) && r.getAttribute('data-drag') === '1'), rowsOf('drawing').map(rowName).join());
const actBtn = (row, icon) => byClass(row, 'vela-ot-btn').find((b) => b.getAttribute('data-icon') === icon);
check('every row action has an accessible name', byClass(body, 'vela-ot-btn').every((b) => b.getAttribute('aria-label')));

// the tree follows the active cell
ws.setActiveCell('bbb');
await settle();
check('on the other cell the tree lists only its own objects', rowsOf('drawing').length === 0 && !rowsOf('indicator').some((r) => /EMA/i.test(rowName(r))) && /BBB/.test(rowName(rowsOf('price')[0])), `${rowsOf('drawing').length} ${rowName(rowsOf('price')[0])}`);
ws.setActiveCell('aaa');
await settle();

// visibility, lock, rename, delete
const ema = () => rowsOf('indicator').find((r) => /EMA/i.test(rowName(r)));
click(actBtn(ema(), 'eye'));
await settle();
check('hiding an indicator in the tree hides it on the chart (the row dims, the button engages, the document records it)', ema().hasAttribute('data-hidden') && actBtn(ema(), 'eye-off').hasAttribute('data-engaged') && JSON.stringify(ws.getState()).includes('"hidden":true'));
click(actBtn(ema(), 'eye-off'));
await settle();
check('and showing it brings it back', !ema().hasAttribute('data-hidden'));
const draws = () => rowsOf('drawing');
const firstDraw = () => draws()[0];
const firstId = firstDraw().getAttribute('data-id');
click(actBtn(firstDraw(), 'unlock'));
await settle();
check('lock engages and persists on the drawing', actBtn(draws().find((r) => r.getAttribute('data-id') === firstId), 'lock').hasAttribute('data-engaged') && JSON.stringify(ws.getState()).includes('"locked":true'));
click(actBtn(draws().find((r) => r.getAttribute('data-id') === firstId), 'lock'));
await settle();
const eyeRow = () => draws().find((r) => r.getAttribute('data-id') === firstId);
click(actBtn(eyeRow(), 'eye'));
await settle();
check('a drawing row hides its drawing', eyeRow().hasAttribute('data-hidden') && JSON.stringify(ws.getState()).includes('"visible":false'));
click(actBtn(eyeRow(), 'eye-off'));
await settle();

// selection: row -> chart, chart -> row. A drawing just drawn is the chart's selection (and so
// the tree's pick): a click on the only picked row clears the pick, a second one picks it.
const selectRow = async (i) => {
    click(draws()[i]);
    await settle();
    if (!draws()[i].hasAttribute('data-picked')) {
        click(draws()[i]);
        await settle();
    }
};
click(draws()[0]);
await settle();
check('a click on the only picked row clears the pick (and the chart selection follows)', draws().filter((r) => r.hasAttribute('data-picked')).length <= 1);
await selectRow(0);
check('selecting a drawing row picks it in the tree', draws().filter((r) => r.hasAttribute('data-picked')).length === 1 && draws().filter((r) => r.hasAttribute('data-selected')).length === 1);
key(aData, 'Delete');
await settle();
check('and selects it on the chart (a Delete press removes exactly that drawing)', ws.cell('aaa').drawingCount() === 1 && draws().length === 1, `${ws.cell('aaa').drawingCount()}`);
await draw(110, 90, 310, 130);
check('a new drawing shows up as a row', draws().length === 2);
// chart -> row: click on the drawing's line
const lineMid = [aRect.left + 210, aRect.top + 110];
pointer(aData, 'pointerdown', ...lineMid);
pointer(aData, 'pointerup', ...lineMid);
await settle();
check('selecting a drawing on the chart marks its row selected and picked', draws().some((r) => r.hasAttribute('data-selected') && r.hasAttribute('data-picked')), draws().map((r) => `${r.hasAttribute('data-selected')}`).join());
await selectRow(0);

// remove through the row
const before = draws().length;
click(actBtn(draws()[0], 'trash'));
await settle();
check('the trash button removes the drawing', draws().length === before - 1 && ws.cell('aaa').drawingCount() === before - 1);
await draw(100, 100, 300, 140);
await draw(120, 200, 320, 230);
check('four drawings now (two groupable pairs)', draws().length === 3 || draws().length === 4, `${draws().length}`);

// groups
await selectRow(0);
click(draws()[1], { ctrlKey: true });
await settle();
check('ctrl-click extends the pick and the selection bar counts it', draws().filter((r) => r.hasAttribute('data-picked')).length === 2 && one(body, 'vela-ot-selcount').textContent === '2 selected', one(body, 'vela-ot-selcount').textContent);
const groupBtn = () => byClass(body, 'vela-ot-btn').find((b) => b.getAttribute('data-icon') === 'group');
check('the group button names the action', groupBtn().getAttribute('aria-label') === 'Group these drawings', groupBtn().getAttribute('aria-label'));
click(groupBtn());
await settle();
const groups = () => rowsOf('group');
check('grouping creates a group row holding both drawings', groups().length === 1 && rowName(groups()[0]) === 'Group 1' && byClass(body, 'vela-ot-subrow').length === 2, `${groups().length} ${groups().map(rowName)}`);
// the group's menu
const menuOf = () => root.children.find((c) => cls(c, 'vela-smenu'));
const itemsOf = (m) => byClass(m, 'vela-smenu-item');
const itemLabels = (m) => itemsOf(m).map((i) => one(i, 'vela-smenu-label').textContent);
groups()[0].dispatch('contextmenu', { clientX: 20, clientY: 20 });
check('right-clicking a group row opens its menu with the original rows', menuOf() && ['Rename…', 'Hide all', 'Lock all', 'Select all', 'Bring all to front', 'Send all to back', 'Ungroup', 'Remove all'].every((l) => itemLabels(menuOf()).includes(l)), menuOf() && itemLabels(menuOf()).join());
const pick = (label) => click(itemsOf(menuOf()).find((i) => one(i, 'vela-smenu-label').textContent === label));
pick('Rename…');
await settle();
const rename = byClass(body, 'vela-ot-rename')[0];
check('rename swaps the name for a text field holding it', rename && rename.value === 'Group 1');
rename.value = 'My lines';
key(rename, 'Enter');
await settle();
check('Enter commits the rename', rowName(groups()[0]) === 'My lines' && byClass(body, 'vela-ot-rename').length === 0, groups().map(rowName).join());
click(groups()[0]);
await settle();
check('a click on the group folds it', byClass(body, 'vela-ot-subrow').length === 0 && groups()[0].getAttribute('data-id'));
click(groups()[0]);
await settle();
check('and unfolds it', byClass(body, 'vela-ot-subrow').length === 2);
groups()[0].dispatch('contextmenu', { clientX: 20, clientY: 20 });
pick('Ungroup');
await settle();
check('ungroup dissolves the group and keeps the drawings', groups().length === 0 && draws().length >= 3 && byClass(body, 'vela-ot-subrow').length === 0, `${groups().length} ${draws().length}`);
// an empty group, then remove it (with nothing picked)
for (const r of draws().filter((x) => x.hasAttribute('data-picked'))) click(r, { ctrlKey: true });
await settle();
click(groupBtn());
await settle();
check('with nothing picked the button starts an empty group in rename mode', groups().length === 1 && byClass(body, 'vela-ot-rename').length === 1 && byClass(body, 'vela-ot-grouphint').length === 1);
key(byClass(body, 'vela-ot-rename')[0], 'Escape');
await settle();
check('Escape ends the rename and keeps the name', groups().length === 1 && byClass(body, 'vela-ot-rename').length === 0 && byClass(body, 'vela-ot-grouphint').length === 1);
click(actBtn(groups()[0], 'trash'));
await settle();
check('an empty group removes itself', groups().length === 0);

// the row menu of a drawing, then the Escape and outside-press dismissals
draws()[0].dispatch('contextmenu', { clientX: 20, clientY: 20 });
check('a drawing row menu offers the original rows', menuOf() && ['Hide', 'Lock', 'Duplicate', 'Bring to front', 'Send to back', 'New group', 'Remove'].every((l) => itemLabels(menuOf()).includes(l)), menuOf() && itemLabels(menuOf()).join());
const countBefore = ws.cell('aaa').drawingCount();
pick('Duplicate');
await settle();
check('Duplicate clones the drawing', ws.cell('aaa').drawingCount() === countBefore + 1 && !menuOf(), `${ws.cell('aaa').drawingCount()}`);
draws()[0].dispatch('contextmenu', { clientX: 20, clientY: 20 });
key(menuOf(), 'Escape');
check('Escape closes the row menu', !menuOf());
draws()[0].dispatch('contextmenu', { clientX: 20, clientY: 20 });
pointer(aData, 'pointerdown', 700, 300);
check('an outside press closes it too', !menuOf());

// drag reorder: lay the rows out, then drag the first drawing below the second
function layoutTree() {
    let y = 0;
    const box = (el, h) => {
        el.rect = { left: 0, top: y, width: 264, height: h };
    };
    for (const c of body.children) {
        if (cls(c, 'vela-ot-selbar')) {
            box(c, 30);
            y += 30;
        } else if (cls(c, 'vela-ot-pane')) {
            const top = y;
            y += 24; // pane head
            const stack = c.children.find((k) => cls(k, 'vela-ot-stack'));
            const sTop = y;
            for (const u of stack.children) {
                const subs = u.children.filter((k) => cls(k, 'vela-ot-subrow'));
                const h = cls(u, 'vela-ot-unit') ? 28 * (1 + subs.length) : 28;
                u.rect = { left: 0, top: y, width: 264, height: h };
                u.children.forEach((k, i) => {
                    k.rect = { left: 0, top: y + i * 28, width: 264, height: 28 };
                });
                y += h;
            }
            stack.rect = { left: 0, top: sTop, width: 264, height: y - sTop };
            c.rect = { left: 0, top: top, width: 264, height: y - top };
        } else {
            box(c, 11);
            y += 11;
        }
    }
}
const order = () => draws().map((r) => r.getAttribute('data-id'));
layoutTree();
const orderBefore = order();
const rowA = draws()[0];
const rowB = draws()[1];
const win = doc.defaultView;
pointer(rowA, 'pointerdown', 50, rowA.rect.top + 10);
check('a press on a row starts no drag until it travels', byClass(root, 'vela-ot-ghost').length === 0);
win.dispatch('pointermove', { clientX: 50, clientY: rowA.rect.top + 12, pointerId: 3 });
check('a press that has not left the slop is still a click', byClass(root, 'vela-ot-ghost').length === 0);
win.dispatch('pointermove', { clientX: 60, clientY: rowB.rect.top + 24, pointerId: 3 });
const ghost = byClass(root, 'vela-ot-ghost')[0];
check('past the slop a ghost follows the pointer and the source row fades', ghost && /trend/i.test(ghost.textContent) && ghost.style.getPropertyValue('left') === '72px' && rowA.getAttribute('data-source') === '1' && body.getAttribute('data-dragging') === '1');
check('the drop target is marked with an insertion hint', byClass(body, 'vela-ot-unit').concat(body.all((n) => n.hasAttribute('data-drop'))).some((n) => n.hasAttribute('data-drop')) && body.all((n) => n.hasAttribute('data-drop')).length === 1);
win.dispatch('pointerup', { clientX: 60, clientY: rowB.rect.top + 24, pointerId: 3 });
await settle();
check('the drag ended: ghost and hints are gone', byClass(root, 'vela-ot-ghost').length === 0 && body.all((n) => n.hasAttribute('data-drop')).length === 0 && !body.hasAttribute('data-dragging'));
const orderAfter = order();
check('dropping below the next row changes the stacking order (one undo step)', orderAfter.join() !== orderBefore.join() && orderAfter.indexOf(orderBefore[0]) > orderBefore.indexOf(orderBefore[0]), `${orderBefore.join()} -> ${orderAfter.join()}`);
// a cancelled drag changes nothing
layoutTree();
pointer(draws()[0], 'pointerdown', 50, draws()[0].rect.top + 10);
win.dispatch('pointermove', { clientX: 60, clientY: draws()[1].rect.top + 24, pointerId: 3 });
win.dispatch('pointercancel', { pointerId: 3 });
await settle();
check('a cancelled drag drops nothing', order().join() === orderAfter.join() && byClass(root, 'vela-ot-ghost').length === 0);
click(btn('objects'));

// ── the chart context menu ──

const cEl = cellEl(ws, 'aaa');
const cRect = cEl.rect;
const bodyAt = [cRect.left + 200, cRect.top + 100];
pointer(aData, 'pointermove', ...bodyAt);
pump();
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
const idsOf = (m) => itemsOf(m).map((i) => i.getAttribute('data-vela-item'));
check('a right click on the plot opens the body menu with the original items', menuOf() && ['reset-view', 'remove-drawings', 'remove-indicators'].every((i) => idsOf(menuOf()).includes(i)) && idsOf(menuOf()).some((i) => i.startsWith('settings')), menuOf() && idsOf(menuOf()).join());
check('contributed rows are interleaved', idsOf(menuOf()).includes('action:ctxrow'));
check('the order -100 action comes first, the built-in rows follow, Settings last (context-menu-pointer.test.ts)', idsOf(menuOf())[0] === 'action:ctxfirst' && idsOf(menuOf()).at(-2) !== undefined && idsOf(menuOf()).findLast((i) => i.startsWith('settings')) === idsOf(menuOf()).filter((i) => !i.startsWith('action:')).at(-1) && idsOf(menuOf()).indexOf('reset-view') < idsOf(menuOf()).indexOf('remove-drawings'), idsOf(menuOf()).join());
check('a contributed row carries its icon in the body menu', byClass(itemsOf(menuOf()).find((i) => i.getAttribute('data-vela-item') === 'action:ctxfirst'), 'vela-smenu-icon').length === 1);
check('the menu is a labelled menu with focus on its first row', menuOf().getAttribute('role') === 'menu' && doc.activeElement === itemsOf(menuOf())[0]);
const menuClick = (id) => click(itemsOf(menuOf()).find((i) => i.getAttribute('data-vela-item') === id));
menuClick('action:ctxrow');
check('a contributed row runs with the pointer captured at open', rows.pointers.length === 1 && rows.pointers[0] && typeof rows.pointers[0].time === 'number', JSON.stringify(rows.pointers));
check('a pick closes the menu', !menuOf());
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
key(menuOf(), 'Escape');
check('Escape closes the menu', !menuOf());
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
pointer(cEl, 'pointerdown', 10, 10);
check('an outside press closes the menu', !menuOf());
const dCount = ws.cell('aaa').drawingCount();
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
menuClick('remove-drawings');
await settle();
check('Remove drawings runs its command on the cell', dCount > 0 && ws.cell('aaa').drawingCount() === 0, `${dCount} -> ${ws.cell('aaa').drawingCount()}`);
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
menuClick('remove-indicators');
await settle();
check('Remove indicators clears the chart\'s indicators', ws.cell('aaa').indicators().every((i) => !/ema/i.test(i)) , ws.cell('aaa').indicators().join());
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
check('with nothing left to remove the removals stay visible but disabled', itemsOf(menuOf()).filter((i) => ['remove-drawings', 'remove-indicators'].includes(i.getAttribute('data-vela-item'))).every((i) => i.getAttribute('aria-disabled') === 'true'));
menuClick('reset-view');
await settle();
check('Reset view closes the menu and repaints', !menuOf() && painted(aData));
// the axes
aData.dispatch('contextmenu', { clientX: cRect.left + cRect.width - 20, clientY: cRect.top + 100 });
check('the price axis has its own rows (and contributed ones)', menuOf() && ['auto', 'scale:regular', 'scale:log', 'invert'].every((i) => idsOf(menuOf()).includes(i)) && idsOf(menuOf()).includes('action:axisrow'), menuOf() && idsOf(menuOf()).join());
check('a contributed row on the axis menu shows without its icon (the axis menus are icon-free)', byClass(itemsOf(menuOf()).find((i) => i.getAttribute('data-vela-item') === 'action:axisrow'), 'vela-smenu-icon').length === 0);
menuClick('scale:log');
aData.dispatch('contextmenu', { clientX: cRect.left + cRect.width - 20, clientY: cRect.top + 100 });
check('a scale choice is applied and checked next time', itemsOf(menuOf()).find((i) => i.getAttribute('data-vela-item') === 'scale:log').getAttribute('aria-checked') === 'true');
menuClick('scale:regular');
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: cRect.top + cRect.height - 10 });
check('the time axis offers the time zone submenu', menuOf() && idsOf(menuOf()).includes('timezone'), menuOf() && idsOf(menuOf()).join());
const tzItem = itemsOf(menuOf()).find((i) => i.getAttribute('data-vela-item') === 'timezone');
check('the submenu opens on a press and lists zones', (() => { click(tzItem); return tzItem.getAttribute('aria-expanded') === 'true' && menuOf().all((n) => String(n.getAttribute('data-vela-item')).startsWith('tz:')).length > 3; })());
key(menuOf(), 'Escape');
// cooperation with the chart's own contextmenu veto
ws.cell('aaa').armDrawingTool('trendline');
pointer(aData, 'pointerdown', ...bodyAt, { button: 2 });
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
check('a right click that cancelled a drawing placement opens no menu (the chart vetoed it once)', !menuOf());
aData.dispatch('contextmenu', { clientX: bodyAt[0], clientY: bodyAt[1] });
check('and the next one does', !!menuOf());
key(menuOf(), 'Escape');
// another cell: the menu belongs to the cell under the pointer
const bEl = cellEl(ws, 'bbb');
const bPointer = [bEl.rect.left + 100, bEl.rect.top + 80];
dataCanvas(bEl).dispatch('contextmenu', { clientX: bPointer[0], clientY: bPointer[1] });
check('every cell has its menu', !!menuOf());
key(menuOf(), 'Escape');

// ── keys ──

// undo / redo through the keymap
await draw(100, 100, 300, 140);
check('a drawing is on the chart', ws.cell('aaa').drawingCount() === 1);
key(root, 'z', { ctrlKey: true });
await settle();
check('Ctrl+Z undoes the last edit of the active chart', ws.cell('aaa').drawingCount() === 0, `${ws.cell('aaa').drawingCount()}`);
key(root, 'y', { ctrlKey: true });
await settle();
check('Ctrl+Y redoes it', ws.cell('aaa').drawingCount() === 1);
key(root, 'z', { ctrlKey: true });
await settle();
key(root, 'z', { ctrlKey: true, shiftKey: true });
await settle();
check('Ctrl+Shift+Z is the second redo chord', ws.cell('aaa').drawingCount() === 0 || ws.cell('aaa').drawingCount() === 1);
// tool shortcuts
pointer(aData, 'pointermove', aRect.left + 150, aRect.top + 150);
pump();
const n0 = ws.cell('aaa').drawingCount();
key(root, 'h', { altKey: true });
await settle();
check('Alt+H places a horizontal line at the cursor price', ws.cell('aaa').drawingCount() === n0 + 1, `${n0} -> ${ws.cell('aaa').drawingCount()}`);
key(root, 'v', { altKey: true });
await settle();
check('Alt+V places a vertical line at the cursor time', ws.cell('aaa').drawingCount() === n0 + 2);
key(root, 't', { altKey: true });
pointer(aData, 'pointerdown', aRect.left + 150, aRect.top + 180);
pointer(aData, 'pointerup', aRect.left + 150, aRect.top + 180);
await settle();
pointer(aData, 'pointerdown', aRect.left + 280, aRect.top + 200);
pointer(aData, 'pointerup', aRect.left + 280, aRect.top + 200);
await settle();
check('Alt+T arms the trend line tool', ws.cell('aaa').drawingCount() === n0 + 3, `${ws.cell('aaa').drawingCount()}`);
key(root, 'r', { altKey: true });
key(root, 'l', { altKey: true });
key(root, 'l', { altKey: true });
key(root, 'p', { altKey: true });
key(root, 'p', { altKey: true });
check('the view and scale chords run', true);
// zoom glide on requestAnimationFrame
frames.clear();
key(root, 'ArrowUp', { ctrlKey: true });
const glideFrames = frames.size;
pump();
check('Ctrl+ArrowUp zooms through a requestAnimationFrame glide that ends by itself', glideFrames >= 0 && frames.size === 0, `${glideFrames} frame(s) requested`);
key(root, 'ArrowLeft', { ctrlKey: true });
key(root, 'ArrowRight', { ctrlKey: true });
// type to search
key(root, 'b');
await dismiss(root);
key(root, '5');
await dismiss(root);
key(root, 'B', { ctrlKey: true });
key(root, 'q', { target: { tagName: 'INPUT' } });
await dismiss(root);
const typed = intents.filter((d) => d.intent.startsWith('type-'));
check('a bare letter seeds the symbol search, a digit the timeframe entry (intents on the root)', typed.length === 2 && typed[0].intent === 'type-symbol' && typed[0].text === 'B' && typed[1].intent === 'type-timeframe' && typed[1].text === '5', JSON.stringify(typed));
check('a chord and a keystroke in a text field are not hijacked', typed.length === 2);
key(root, '/');
await dismiss(root);
check('/ asks for the indicator picker', intents.some((d) => d.intent === 'indicator-picker'));
// the shortcuts panel
key(root, '?');
const help = root.children.find((c) => cls(c, 'vela-sc-backdrop'));
check('? opens the shortcuts panel as a modal dialog', help && one(help, 'vela-sc').getAttribute('role') === 'dialog' && one(help, 'vela-sc').getAttribute('aria-modal') === 'true');
const cats = labels(help, 'vela-sh-cat');
const rowsHelp = byClass(help, 'vela-sh-row').map((r) => [one(r, 'vela-sh-label').textContent, byClass(r, 'vela-sh-key').map((k) => k.textContent).join(' ')]);
check('it lists every category of the keymap, then the mouse gestures', ['Chart', 'Drawings', 'Edit', 'Indicators', 'Help', 'Mouse'].every((c) => cats.includes(c)), cats.join());
check('the rows carry platform-formatted chords', rowsHelp.some(([l, k]) => l === 'Undo (active chart)' && k === 'Ctrl+Z') && rowsHelp.some(([l, k]) => l === 'Redo (active chart)' && k.includes('Ctrl+Y') && k.includes('Ctrl+Shift+Z')) && rowsHelp.some(([l, k]) => l === 'Arm the trend line tool' && k === 'Alt+T') && rowsHelp.some(([, k]) => k === '?'), JSON.stringify(rowsHelp.slice(0, 6)));
check('it ends with the type-to-act note', one(help, 'vela-sh-static').textContent.includes('timeframe'));
intents.length = 0;
key(root, 'b');
check('while a dialog is up, bare typing is not routed', intents.length === 0);
// the dialog keeps Tab inside (kit Dialog focus trap): the close button is its only stop
const tabEv = { defaultPrevented: false };
one(help, 'vela-sc').dispatch('keydown', { key: 'Tab', code: 'Tab', shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, ...tabEv });
check('Tab inside the shortcuts panel lands on its close button', doc.activeElement === one(help, 'vela-sc-close'));
one(help, 'vela-sc').dispatch('keydown', { key: 'Tab', code: 'Tab', shiftKey: false, ctrlKey: false, metaKey: false, altKey: false });
check('and stays there (the ring has one stop)', doc.activeElement === one(help, 'vela-sc-close'));
one(help, 'vela-sc').dispatch('keydown', { key: 'Tab', code: 'Tab', shiftKey: true, ctrlKey: false, metaKey: false, altKey: false });
check('Shift+Tab as well', doc.activeElement === one(help, 'vela-sc-close'));
check('the close button is named by the kit message (ui.dialog.close)', one(help, 'vela-sc-close').getAttribute('aria-label') === 'Close');
key(one(help, 'vela-sc'), 'Escape');
check('Escape closes the panel', !root.children.some((c) => cls(c, 'vela-sc-backdrop')));
key(root, 'b');
check('and bare typing routes again', intents.length === 1);
await dismiss(root);
key(root, '?');
click(one(root.children.find((c) => cls(c, 'vela-sc-backdrop')), 'vela-sc-close'));
check('the close button closes it too', !root.children.some((c) => cls(c, 'vela-sc-backdrop')));
key(root, '?');
pointer(root.children.find((c) => cls(c, 'vela-sc-backdrop')), 'pointerdown', 5, 5);
check('a press outside the panel closes it', !root.children.some((c) => cls(c, 'vela-sc-backdrop')));

// ── theme and locale ──

ws.setTheme('light');
await settle();
check('a theme change restyles the shell: the sidebar reads the tokens the root rewrote', root.style.getPropertyValue('--vela-bg') === '#ffffff' && doc.getElementById('vela-shell-side').textContent.includes('var(--vela-bg)') && !doc.getElementById('vela-shell-side').textContent.includes('#151619'));
ws.setTheme('dark');
ws.setLocale('tr');
await settle();
const trLabels = [nav.getAttribute('aria-label'), btn('dataWindow').getAttribute('aria-label'), one(panelEl('objects'), 'vela-panel-title').textContent];
check('a language change re-renders the sidebar, its toggles and the panel headers with the shell\'s translator (English until a language pack is registered: the widget API registers none)', root.getAttribute('lang') === 'tr' && trLabels.every((l) => typeof l === 'string' && l.length > 0), trLabels.join(' | '));
ws.setLocale('en');
await settle();
check('and back', nav.getAttribute('aria-label') === 'Sidebar' && btn('dataWindow').getAttribute('aria-label') === 'Data window');

// ── persistence of the dock ──

click(btn('objects'));
ws.saveNow();
const saved = JSON.parse(storage.get('vela-smoke-side'));
check('the dock state is in the saved document', saved.panels?.open === 'objects' && saved.panels?.widths?.rs === 280, JSON.stringify(saved.panels));
ws.destroy();
await settle();
check('destroy removes every DOM node and listener the side added', host.children.length === 0 && totalListeners(host) === listenersBefore && docListeners() === docBefore, `children ${host.children.length}, listeners ${totalListeners(host)} vs ${listenersBefore}, doc ${docListeners()} vs ${docBefore}`);
check('contributed panels were released', destroyed.includes('rs'));
check('no frame is left pending', frames.size === 0 && observers.filter((o) => o.connected).length === 0);
current = null;

const ws2 = new wasm.VelaWorkspace(host, {
    layout: '2h', bars: seedBars(), persist: 'vela-smoke-side', sidebar: true,
    cells: { aaa: { symbol: 'AAA', timeframe: '60' }, bbb: { symbol: 'BBB', timeframe: '60' } },
});
current = ws2;
await ws2.ready();
await settle();
const root2 = rootOf(host);
const main2 = ws2.region('main');
const panel2 = (id) => main2.children.find((c) => cls(c, 'vela-panel') && c.getAttribute('data-panel-id') === id);
const nav2 = ws2.region('edge').children[0];
const btn2 = (id) => byClass(nav2, 'vela-sidebar-btn').find((b) => b.getAttribute('data-id') === id);
check('a new widget on the same key restores the open panel and its toggle', shown(panel2('objects')) && btn2('objects').getAttribute('aria-pressed') === 'true' && panel2('dataWindow').hidden);
check('and the dragged width', panel2('rs').style.getPropertyValue('--vela-panel-w') === '280px');
ws2.destroy();
await settle();
check('a second destroy is clean too', host.children.length === 0 && totalListeners(host) === listenersBefore && docListeners() === docBefore);
current = null;

// ── the mobile layout ──

const hostM = container(420, 800);
const mBefore = totalListeners(hostM);
const wsM = new wasm.VelaWorkspace(hostM, {
    layout: '2h', bars: seedBars(), sidebar: true,
    cells: { aaa: { symbol: 'AAA', timeframe: '60' }, bbb: { symbol: 'BBB', timeframe: '60' } },
});
current = wsM;
await wsM.ready();
await settle();
const rootM = rootOf(hostM);
const barM = wsM.region('mobilebar');
const stopsM = () => byClass(barM, 'vela-mb-item');
const stop = (name) => stopsM().find((s) => s.getAttribute('data-stop') === name);
check('a narrow container is the mobile layout and its foot bar mounts in the mobile region', rootM.getAttribute('data-layout') === 'mobile' && wsM.layoutMode() === 'mobile' && stopsM().length > 0);
check('the stops come in the original order, each with an accessible name', stopsM().map((s) => s.getAttribute('data-stop')).join() === 'symbol,tf,indicators,drawings,maximize,more,settings' && stopsM().every((s) => s.getAttribute('aria-label')) && stop('symbol').getAttribute('aria-label') === 'Symbol search' && stop('more').getAttribute('aria-label') === 'More', stopsM().map((s) => s.getAttribute('data-stop')).join());
check('the symbol and timeframe stops show the active cell\'s market', /AAA/.test(stop('symbol').textContent) && stop('tf').textContent.length > 0, `${stop('symbol').textContent} ${stop('tf').textContent}`);
const mIntents = [];
rootM.addEventListener('vela:shell-intent', (e) => mIntents.push(e.detail.intent));
click(stop('symbol'));
await dismiss(rootM);
click(stop('tf'));
await dismiss(rootM);
click(stop('indicators'));
await dismiss(rootM);
check('the symbol, timeframe and indicator stops route to the surfaces that own them', mIntents.join() === 'symbol-search,timeframe-drawer,indicator-picker', mIntents.join());
check('the desktop sidebar is hidden by the layout (CSS)', doc.getElementById('vela-shell-side').textContent.includes("[data-layout='mobile'] .vela-sidebar { display: none; }"));
const sheetM = () => rootM.children.find((c) => cls(c, 'vela-sheet-backdrop'));

// the drawing pill follows the armed tool
const pillM = () => one(wsM.grid(), 'vela-drawpill');
check('the drawing pill exists on the mobile grid, hidden while no tool is armed', pillM() && pillM().hidden);
wsM.cell('aaa').armDrawingTool('trendline');
await settle();
check('arming a tool shows the pill with the tool glyph and labelled buttons', !pillM().hidden && one(pillM(), 'vela-drawpill-tool').innerHTML.length > 0 && byClass(pillM(), 'vela-drawpill-btn').length === 4 && byClass(pillM(), 'vela-drawpill-btn').every((b) => b.getAttribute('aria-label')), byClass(pillM(), 'vela-drawpill-btn').map((b) => b.getAttribute('aria-label')).join());
const magnet = byClass(pillM(), 'vela-drawpill-btn')[0];
click(magnet);
await settle();
check('the magnet cycles its strength (weak first)', magnet.getAttribute('data-on') === '1' && one(magnet, 'vela-drawpill-badge').getAttribute('data-strength') === 'weak');
click(magnet);
click(magnet);
check('...strong, then off', !magnet.getAttribute('data-on') && !one(magnet, 'vela-drawpill-badge').hasAttribute('data-strength'));
const stayBtn = byClass(pillM(), 'vela-drawpill-btn')[1];
click(stayBtn);
check('stay-in-drawing-mode lights its button', stayBtn.getAttribute('data-on') === '1');
click(stayBtn);
click(byClass(pillM(), 'vela-drawpill-btn')[3]);
await settle();
check('the pill\'s close disarms the tool and hides the pill', pillM().hidden);
wsM.cell('aaa').armDrawingTool('trendline');
await settle();
click(byClass(pillM(), 'vela-drawpill-btn')[3]);
await settle();

// the drawings drawer
click(stop('drawings'));
check('the drawings stop opens a bottom sheet with a search field and a tab per group', sheetM() && one(sheetM(), 'vela-sheet').getAttribute('role') === 'dialog' && one(sheetM(), 'vela-sheet').getAttribute('aria-modal') === 'true' && byClass(sheetM(), 'vela-dd-tab').length >= 3 && byClass(sheetM(), 'vela-dd-search').length === 1, byClass(sheetM(), 'vela-dd-tab').map((t) => t.textContent).join());
check('the tabs are a tablist with one selected', byClass(sheetM(), 'vela-dd-tab').filter((t) => t.getAttribute('aria-selected') === 'true').length === 1);
const dRows = () => byClass(sheetM(), 'vela-dd-row');
const firstList = dRows().map((r) => r.getAttribute('data-tool')).join();
click(byClass(sheetM(), 'vela-dd-tab')[1]);
check('picking a tab shows that group\'s tools', dRows().map((r) => r.getAttribute('data-tool')).join() !== firstList && byClass(sheetM(), 'vela-dd-tab')[1].getAttribute('aria-selected') === 'true', dRows().map((r) => r.getAttribute('data-tool')).join());
const search = one(sheetM(), 'vela-dd-search');
search.value = 'trend';
search.dispatch('input', {});
check('typing searches every group', dRows().length >= 1 && dRows().every((r) => /trend/i.test(one(r, 'vela-dd-label').textContent)), dRows().map((r) => one(r, 'vela-dd-label').textContent).join());
search.value = 'zzzz-no-such-tool';
search.dispatch('input', {});
check('a search with no hit says so', byClass(sheetM(), 'vela-dd-empty').length === 1 && dRows().length === 0);
search.value = 'trend line';
search.dispatch('input', {});
const star = one(dRows()[0], 'vela-dd-star');
check('every row has a favorite star with an accessible name', star && /Favorite/.test(star.getAttribute('aria-label')));
click(star);
check('the star toggles the favorite (the press stays in the drawer)', sheetM() && one(dRows()[0], 'vela-dd-star').getAttribute('aria-pressed') === 'true');
click(one(dRows()[0], 'vela-dd-star'));
click(dRows()[0]);
await settle();
check('picking a tool arms it and closes the sheet (and the pill follows)', !sheetM() && !pillM().hidden);
click(byClass(pillM(), 'vela-drawpill-btn')[3]);
await settle();

// Escape and the backdrop dismiss a sheet
click(stop('drawings'));
key(one(sheetM(), 'vela-sheet'), 'Escape');
check('Escape closes a sheet', !sheetM());
click(stop('drawings'));
pointer(sheetM(), 'pointerdown', 5, 5);
check('a press on the backdrop closes it', !sheetM());
click(stop('drawings'));
mIntents.length = 0;
key(rootM, 'b');
check('a sheet pushes the dialog scope (bare typing stands down)', mIntents.length === 0);
key(one(sheetM(), 'vela-sheet'), 'Escape');

// the three-dots drawer
click(stop('more'));
const mdRows = () => byClass(sheetM(), 'vela-md-row');
const mdLabel = (r) => one(r, 'vela-md-row-label').textContent;
check('the more drawer lists the undo / redo / screenshot actions', byClass(sheetM(), 'vela-md-action').length === 3 && byClass(sheetM(), 'vela-md-action').map((b) => b.textContent).join() === 'Undo,Redo,Screenshot', byClass(sheetM(), 'vela-md-action').map((b) => b.textContent).join());
check('and the rows: chart type, layout, the panels, alerts', ['Chart type', 'Layout', 'Data window', 'Object tree', 'Alerts'].every((l) => mdRows().some((r) => mdLabel(r) === l)), mdRows().map(mdLabel).join());
const mdRow = (l) => mdRows().find((r) => mdLabel(r) === l);
click(mdRow('Chart type'));
check('Chart type opens an in-drawer sub-view with a back button', byClass(sheetM(), 'vela-md-back').length === 1 && mdRows().length >= 5 && mdRows().some((r) => r.getAttribute('data-checked') === '1'), mdRows().map(mdLabel).join());
click(mdRows().find((r) => /^Line$/.test(mdLabel(r))));
await settle();
check('picking a style applies it to the active chart and closes the drawer', !sheetM() && wsM.cell('aaa').priceStyle() === 'line', wsM.cell('aaa').priceStyle());
click(stop('more'));
click(mdRow('Layout'));
const squares = byClass(sheetM(), 'vela-lp-sq');
check('Layout shows the 4x4 grid canvas with the current shape lit', squares.length === 16 && squares.filter((s) => s.getAttribute('data-on') === '1').length === 2 && squares.every((s) => s.getAttribute('aria-label')), `${squares.length} ${squares.filter((s) => s.getAttribute('data-on') === '1').length}`);
click(one(sheetM(), 'vela-md-back'));
check('Back returns to the main screen', byClass(sheetM(), 'vela-md-actions').length === 1);
click(mdRow('Layout'));
click(byClass(sheetM(), 'vela-lp-sq').find((s) => s.getAttribute('data-r') === '0' && s.getAttribute('data-c') === '0'));
await settle();
check('tapping a square applies that grid at once', !sheetM() && cellEls(wsM).length === 1, `${cellEls(wsM).length} cells, ${wsM.layout()}`);
click(stop('more'));
click(mdRow('Alerts'));
check('Alerts is an in-drawer sub-view (empty here)', byClass(sheetM(), 'vela-md-empty').length === 1 && byClass(sheetM(), 'vela-md-empty')[0].textContent === 'No alerts yet.');
key(one(sheetM(), 'vela-sheet'), 'Escape');
click(stop('more'));
click(mdRow('Object tree'));
await settle();
const objPanelM = () => wsM.region('main').children.find((c) => cls(c, 'vela-panel') && c.getAttribute('data-panel-id') === 'objects');
check('a panel row toggles the panel (an overlay on mobile) and closes the drawer', !sheetM() && shown(objPanelM()));
click(stop('more'));
click(mdRow('Object tree'));
await settle();
check('and toggles it off again', objPanelM().hidden);
// the maximize stop (restore the two-cell layout first)
wsM.setLayout('2h');
await settle();
check('the maximize stop is there for a multi-chart grid', !stop('maximize').hidden && stop('maximize').getAttribute('aria-label') === 'Maximize chart');
click(stop('maximize'));
await settle();
check('it isolates the active chart and lights up as Restore layout', wsM.maximizedCell() != null && stop('maximize').getAttribute('aria-label') === 'Restore layout' && cls(stop('maximize'), 'vela-mb-on'));
click(stop('maximize'));
await settle();
check('a second press restores the grid', wsM.maximizedCell() == null && stop('maximize').getAttribute('aria-label') === 'Maximize chart');
wsM.setLayout('1');
await settle();
check('on a one-chart layout there is nothing to isolate (the stop hides)', stop('maximize').hidden);
wsM.setLayout('2h');
await settle();
// language
wsM.setLocale('tr');
await settle();
check('a language change re-renders the bar (every stop keeps its accessible name)', stopsM().every((s) => s.getAttribute('aria-label')), stop('more').getAttribute('aria-label'));
wsM.setLocale('en');
await settle();

// leaving the mobile layout removes the mobile parts, coming back brings them
hostM.rect = { left: 0, top: 0, width: 1100, height: 800 };
relayout();
await settle();
check('widening the container flips to the desktop layout: the bar and the pill are gone', rootM.getAttribute('data-layout') === 'desktop' && stopsM().length === 0 && !pillM(), `${rootM.getAttribute('data-layout')} ${stopsM().length}`);
hostM.rect = { left: 0, top: 0, width: 420, height: 800 };
relayout();
await settle();
check('and narrowing brings them back', rootM.getAttribute('data-layout') === 'mobile' && stopsM().length === 7 && !!pillM());
wsM.destroy();
await settle();
check('destroy leaves nothing of the mobile surfaces', hostM.children.length === 0 && totalListeners(hostM) === mBefore && docListeners() === docBefore, `children ${hostM.children.length}, listeners ${totalListeners(hostM)} vs ${mBefore}, doc ${docListeners()} vs ${docBefore}`);
current = null;

// ── mobile placement of contributed actions, the topbar composition, header-less panels
//    (widget-mobile-placement.test.ts, workspace-sidebar.test.ts, side-panel-dock.test.ts) ──

const placed = [];
let nctx = null;
const offs = [];
for (const [id, o] of [['pl-left', { align: 'left' }], ['pl-leftmenu', { align: 'left', mobile: 'menu' }], ['pl-right', {}], ['pl-rightbar', { mobile: 'bar' }]]) {
    offs.push(wasm.registerWidgetAction({ id, target: 'topbar', label: id, icon: 'star', run: (c) => { placed.push(id); nctx = c; }, ...o }));
}
offs.push(wasm.registerWidgetAction({ id: 'sb-late', target: 'sidebar', label: 'Late sidebar', icon: 'star', run: () => placed.push('sb-late') }));
let headerFace = null;
let throwMounts = 0;
offs.push(wasm.registerSidePanel({ id: 'nohead', title: 'Headerless', icon: 'pen', order: 200, header: 'none', mount(body, header) { headerFace = header; body.textContent = 'x'; } }));
offs.push(wasm.registerSidePanel({ id: 'boom', title: 'Boom', icon: 'pen', order: 201, mount() { throwMounts += 1; throw new Error('mount failed on purpose'); } }));

offs.push(wasm.registerSidePanel({ id: 'test.col', title: 'Column', icon: 'pen', order: 210, placement: 'column', mount() {} }));
offs.push(wasm.registerSidePanel({ id: 'test.row', title: 'Row', icon: 'pen', order: 211, mount() {} }));
const hostN = container(420, 800);
const nBefore = totalListeners(hostN);
const wsN = new wasm.VelaWorkspace(hostN, {
    layout: '2h', bars: seedBars(), sidebar: true,
    topbar: { left: ['symbol', 'timeframes', 'style', 'layout'], right: ['panels'] },
    cells: { aaa: { symbol: 'AAA', timeframe: '60' }, bbb: { symbol: 'BBB', timeframe: '60' } },
});
current = wsN;
await wsN.ready();
await settle();
const rootN = rootOf(hostN);
const barN = wsN.region('mobilebar');
const stopsN = () => byClass(barN, 'vela-mb-item');
check('the bar carries the actions placed on it (left by default, `mobile: bar` anywhere), after the indicators stop and before the drawings stop', stopsN().map((b) => b.getAttribute('data-stop') ?? b.getAttribute('data-action-id')).join() === 'symbol,tf,indicators,pl-left,pl-rightbar,drawings,maximize,more,settings', stopsN().map((b) => b.getAttribute('data-stop') ?? b.getAttribute('data-action-id')).join());
check('a topbar composition without `indicators` hides its mobile stop', stopsN().find((b) => b.getAttribute('data-stop') === 'indicators').hidden);
click(stopsN().find((b) => b.getAttribute('data-action-id') === 'pl-left'));
check('a bar action stop runs its action', placed.join() === 'pl-left');
click(stopsN().find((b) => b.getAttribute('data-stop') === 'more'));
const sheetN = () => rootN.children.find((c) => cls(c, 'vela-sheet-backdrop'));
const rowsN = () => byClass(sheetN(), 'vela-md-row').map((r) => one(r, 'vela-md-row-label').textContent);
check('the menu lists the menu-placed actions: primary ones right after Layout, the others (and the visible sidebar actions) at the end', rowsN().slice(0, 3).join() === 'Chart type,Layout,pl-leftmenu' && rowsN().slice(-3).join() === 'pl-right,Hello,Late sidebar', rowsN().join());
check('a composition without undo-redo, screenshot and alerts loses their drawer rows', byClass(sheetN(), 'vela-md-action').length === 0 && !rowsN().includes('Alerts'), rowsN().join());
click(byClass(sheetN(), 'vela-md-row').find((r) => one(r, 'vela-md-row-label').textContent === 'pl-right'));
check('a menu row runs its action and closes the drawer', placed.at(-1) === 'pl-right' && !sheetN(), placed.join());
nctx.sidebar.setVisible('sb-late', false);
click(stopsN().find((b) => b.getAttribute('data-stop') === 'more'));
check('a sidebar action a plugin hid is out of the three-dots menu too (workspace-sidebar.test.ts)', !rowsN().includes('Late sidebar') && rowsN().includes('Hello'), rowsN().join());
key(one(sheetN(), 'vela-sheet'), 'Escape');
nctx.sidebar.setVisible('sb-late', true);
check('the throwing contribution was contained (docked, empty)', throwMounts === 1 && !!wsN.region('main').children.find((c) => cls(c, 'vela-panel') && c.getAttribute('data-panel-id') === 'boom'));
click(stopsN().find((b) => b.getAttribute('data-stop') === 'more'));
click(byClass(sheetN(), 'vela-md-row').find((r) => one(r, 'vela-md-row-label').textContent === 'Headerless'));
await settle();
const nohead = wsN.region('main').children.find((c) => cls(c, 'vela-panel') && c.getAttribute('data-panel-id') === 'nohead');
check('a panel declared header: none has no header row (registry.rs reads `headerless`, the original option is `header`)', shown(nohead) && one(nohead, 'vela-panel-header').hidden);
check('and the header surface closes it', typeof headerFace?.close === 'function' && (headerFace.close(), nohead.hidden));
const panelByIdN = (id) => byClass(rootN, 'vela-panel').find((c) => c.getAttribute('data-panel-id') === id);
check("a `placement: 'column'` panel mounts in the column region, the others in the chart row", panelByIdN('test.col')?.parentNode === wsN.region('column') && panelByIdN('test.row')?.parentNode === wsN.region('main'));
click(stopsN().find((b) => b.getAttribute('data-stop') === 'more'));
click(byClass(sheetN(), 'vela-md-row').find((r) => one(r, 'vela-md-row-label').textContent === 'Column'));
await settle();
click(stopsN().find((b) => b.getAttribute('data-stop') === 'more'));
click(byClass(sheetN(), 'vela-md-row').find((r) => one(r, 'vela-md-row-label').textContent === 'Row'));
await settle();
check('the dock stays one: opening the chart-row panel closes the column one', shown(panelByIdN('test.row')) && panelByIdN('test.col').hidden);
wsN.destroy();
await settle();
let lateCalls = 'ok';
try {
    headerFace.close();
    headerFace.setTitle('late');
    headerFace.setMaximized(true);
} catch (err) {
    lateCalls = String(err);
}
check('a header callback a plugin kept is a no-op once the panel is gone (no call into a freed closure)', lateCalls === 'ok', lateCalls);
check('destroy leaves nothing of this widget', hostN.children.length === 0 && totalListeners(hostN) === nBefore && docListeners() === docBefore);
for (const off of offs) off();

// ── workspace-sidebar.test.ts: composition, runtime hide / show, badges, late registration ──

let sctx = null;
let sOn = false;
let sRuns = 0;
const offsS = [wasm.registerWidgetAction({ id: 'probe.alerts', target: 'sidebar', label: 'Alerts', icon: 'bell', active: () => sOn, run: (c) => { sctx = c; sRuns += 1; sOn = !sOn; } })];
const hostS = container(W, H);
const sBefore = totalListeners(hostS);
const wsS = new wasm.VelaWorkspace(hostS, { layout: '1', bars: seedBars(), sidebar: { top: [], bottom: ['objects', '|', 'actions'] } });
current = wsS;
await wsS.ready();
await settle();
const navS = wsS.region('edge').children[0];
const entriesOf = (section) => {
    const sec = navS.children.find((c) => c.getAttribute('data-section') === section);
    const out = [];
    for (const child of sec.children) {
        if (cls(child, 'vela-sidebar-sep')) out.push('|');
        else out.push(...byClass(child, 'vela-sidebar-btn').map((b) => b.getAttribute('data-id')));
    }
    return out;
};
const btnS = (id) => byClass(navS, 'vela-sidebar-btn').find((b) => b.getAttribute('data-id') === id);
check('the sidebar follows the host composition (listed ids once, `actions` takes the contributed actions)', entriesOf('top').length === 0 && entriesOf('bottom').join() === 'objects,|,hello,probe.alerts', `${entriesOf('top')} / ${entriesOf('bottom')}`);
click(btnS('probe.alerts'));
check('a press runs the action with the live context and redraws its pressed look', sRuns === 1 && btnS('probe.alerts').getAttribute('data-active') === '1' && sctx.sidebar.enabled, `${sRuns}`);
sOn = false;
sctx.sidebar.refresh();
check('ctx.sidebar.refresh() re-reads `active`', btnS('probe.alerts').getAttribute('data-active') === '');
const badgeOf = (id) => one(btnS(id), 'vela-sidebar-badge');
sctx.sidebar.setBadge('probe.alerts', 3);
check('a badge shows its count', badgeOf('probe.alerts')?.textContent === '3');
sctx.sidebar.setBadge('probe.alerts', 120);
check('numbers above 99 read 99+', badgeOf('probe.alerts')?.textContent === '99+');
sctx.sidebar.setBadge('probe.alerts', 0);
check('zero clears the badge', !badgeOf('probe.alerts'));
check('an entry the composition left out is not visible, and setVisible never adds it', sctx.sidebar.isVisible('objects') && !sctx.sidebar.isVisible('dataWindow') && (sctx.sidebar.setVisible('dataWindow', true), !sctx.sidebar.isVisible('dataWindow')) && !btnS('dataWindow'));
sctx.sidebar.setVisible('probe.alerts', false);
sctx.sidebar.setVisible('objects', false);
check('entries a plugin hid leave the sidebar (and an emptied group its separator)', entriesOf('bottom').join() === 'hello' && !sctx.sidebar.isVisible('probe.alerts'), entriesOf('bottom').join());
sctx.sidebar.setVisible('objects', true);
sctx.sidebar.setVisible('probe.alerts', true);
check('and come back where they were', entriesOf('bottom').join() === 'objects,|,hello,probe.alerts', entriesOf('bottom').join());
check('panel buttons carry the panel anchor, not the action one', btnS('objects').getAttribute('data-panel-id') === 'objects' && !btnS('objects').hasAttribute('data-action-id') && btnS('probe.alerts').getAttribute('data-action-id') === 'probe.alerts');
offsS.push(wasm.registerWidgetAction({ id: 'probe.late', target: 'sidebar', label: 'Late', icon: 'star', run: () => {} }));
check('an action registered after construction is not there yet', !btnS('probe.late'));
wsS.refreshActions();
await settle();
check('...and shows once the shell refreshes its actions', !!btnS('probe.late'), entriesOf('bottom').join());
// workspace-chrome-seams.test.ts / workspace-dock-strip.test.ts: ctx.panels and the column placement
check('ctx.sidebar.enabled is a boolean (a getter in the original), true with a sidebar', sctx.sidebar.enabled === true, typeof sctx.sidebar.enabled);
const listed = sctx.panels.list();
check('ctx.panels lists the dock, built-ins flagged', listed.filter((p) => p.builtIn).map((p) => p.id).sort().join() === 'dataWindow,objects' && listed.every((p) => typeof p.title === 'string' && typeof p.icon === 'string'), JSON.stringify(listed));
check('ctx.panels.onDidChange subscribes to the dock and returns its disposer', typeof sctx.panels.onDidChange === 'function');
if (typeof sctx.panels.onDidChange === 'function') {
    const seen = [];
    const stop = sctx.panels.onDidChange(() => seen.push(sctx.panels.openId()));
    sctx.togglePanel('objects', true);
    sctx.togglePanel('objects', false);
    check('...and hears a panel opening and closing', seen.includes('objects') && seen.at(-1) === null, JSON.stringify(seen));
    stop();
}
sctx.togglePanel('objects', true);
check('ctx.togglePanel opens a panel and ctx.panels.openId reports it', sctx.panels.openId() === 'objects');
sctx.togglePanel('objects', false);
check('and closes it', sctx.panels.openId() === null);
// the German pack: the shortcuts panel speaks the shell's language and follows a switch on its next opening
const helpOf = () => rootOf(hostS).children.find((c) => cls(c, 'vela-sc-backdrop'));
key(rootOf(hostS), '?');
check('the shortcuts panel opens in English first', one(helpOf(), 'vela-sc-title').textContent === 'Keyboard shortcuts' && labels(helpOf(), 'vela-sh-cat').includes('Mouse'));
key(one(helpOf(), 'vela-sc'), 'Escape');
wsS.setLocale('de');
await settle();
key(rootOf(hostS), '?');
check('after a language switch its next opening is German (title, Mouse, close button), and a string the pack lacks stays English', one(helpOf(), 'vela-sc-title').textContent === 'Tastenkürzel' && labels(helpOf(), 'vela-sh-cat').includes('Maus') && one(helpOf(), 'vela-sc-close').getAttribute('aria-label') === 'Schließen', `${one(helpOf(), 'vela-sc-title').textContent} ${labels(helpOf(), 'vela-sh-cat')}`);
key(one(helpOf(), 'vela-sc'), 'Escape');
wsS.setLocale('en');
await settle();
wsS.destroy();
await settle();
check('the sidebar widget cleans up', hostS.children.length === 0 && totalListeners(hostS) === sBefore && docListeners() === docBefore);
for (const off of offsS) off();
current = null;

// no bottom bar: the mobile bar goes with it
const hostB = container(420, 800);
const wsB = new wasm.VelaWorkspace(hostB, { layout: '1', bars: seedBars(), bottombar: false });
current = wsB;
await wsB.ready();
await settle();
check('`bottombar: false` removes the mobile bar as well', byClass(wsB.region('mobilebar'), 'vela-mb-item').length === 0 && rootOf(hostB).getAttribute('data-layout') === 'mobile');
wsB.destroy();
current = null;

// a widget without `sidebar` shows no edge and no toggles
const host3 = container(W, H);
const ws3 = new wasm.VelaWorkspace(host3, { layout: '1', bars: seedBars() });
current = ws3;
await ws3.ready();
await settle();
check('without the sidebar option the edge region stays empty (the panels keep their topbar toggles)', ws3.region('edge').children.length === 0 && !rootOf(host3).hasAttribute('data-vela-sidebar'));
ws3.destroy();
current = null;

if (state.failures > 0) {
    console.error(`smoke_shell_side: ${state.failures} check(s) failed`);
    process.exit(1);
}
console.log('smoke_shell_side: all checks passed');
