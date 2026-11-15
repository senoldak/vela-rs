// Smoke test of the ASSEMBLED widget (vela-web `workspace_ui` + `shell_top` + `shell_side`, put
// together by `VelaWidget` / `VelaWorkspace`) in Node, against the minimal fake DOM: what the
// default widget shows, the shell options, the locale packs, the one `Contributions` registry
// and the one `WidgetContext` that the root, the topbar and the side surfaces share, the
// wiring between the top and the side (panel toggles, dialog counters, mobile menu), the
// screenshot (button and chord), the dialogs at the root and the workspace events that
// close the seams (status prefs, restored panels).
//
// The fake DOM has no layout engine: this script plays one (the grid template of the
// workspace smoke). Nothing here runs a browser: real layout, focus, pointer capture, painting
// and CSS are not exercised, and no exchange endpoint is ever called (the in-memory provider
// is the only data source).
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_widget.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, FakeNode, check, clock, container, doc, loadPackage, observers, options, pump, state, storage } from './fake_dom.mjs';

options.captureAncestors = true;
// The fake canvas has no encoder: a PNG data URL naming its backing size stands in.
FakeCanvas.prototype.toDataURL = function toDataURL(type) {
    return `data:${type};size=${this.width}x${this.height}`;
};
// Additive stand-ins the shell reads through plain property access.
Object.defineProperty(FakeNode.prototype, 'lastElementChild', {
    get() {
        return this.children.at(-1) ?? null;
    },
    configurable: true,
});
// An `<a download>` click records the download.
const downloads = [];
FakeNode.prototype.click = function click() {
    if (this.tagName === 'A') downloads.push({ href: this.href, name: this.download });
};

// Additive stand-ins for the strip docking checks: comments and replaceChild.
doc.createComment ??= function createComment(text) {
    const n = new FakeNode('#comment', doc);
    n.data = text;
    return n;
};
FakeNode.prototype.replaceChild ??= function replaceChild(next, old) {
    next.remove();
    const at = this.children.indexOf(old);
    if (at < 0) return old;
    this.children.splice(at, 1, next);
    next.parentNode = this;
    old.parentNode = null;
    return old;
};
const wasm = await loadPackage('smoke_widget.mjs');

const W = 1000;
const H = 600;
const GAP = 2;
const SYMBOLS = ['AAA', 'BBB', 'CCC', 'DDD'];

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
const hasClass = (n, c) => String(n.className).split(/\s+/).includes(c);
const cellsOf = (grid) => grid.children.filter((c) => hasClass(c, 'vela-cell'));
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
    if (current) layoutGrid(typeof current.grid === "function" ? current.grid() : current.root().children.find((c) => hasClass(c, "vela-ws-main")).children[0]);
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

// ── helpers over the fake DOM ──
const byClass = (root, c) => root.all((n) => hasClass(n, c));
const one = (root, c) => byClass(root, c)[0];
const click = (n) => n.dispatch('click', { button: 0 });
const key = (target, k, extra = {}) => target.dispatch('keydown', { key: k, code: '', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });
const text = (n) => n?.textContent ?? '';
const rootOf = (host) => host.children.find((c) => hasClass(c, 'vela-workspace'));
const totalListeners = (node) => node.listeners.length + node.children.reduce((n, c) => n + totalListeners(c), 0);
const labelsOf = (root) => {
    const out = new Set();
    root.all(() => true).forEach((n) => {
        for (const k of ['aria-label', 'title']) {
            const v = n.getAttribute?.(k);
            if (v) out.add(v);
        }
    });
    return out;
};
const cellEl = (ws, id) => cellsOf(ws.grid()).find((c) => c.getAttribute('data-cell-id') === id);
const dataCanvas = (cell) => cell.children[0].children[0].children.find((c) => c.style.getPropertyValue('pointer-events') !== 'none' && c.ctx);
const dialogsOf = (root) => root.all((n) => n.getAttribute?.('role') === 'dialog');

// ═══ contributions: registered BEFORE the widgets (the page-wide registries) ═══

const calls = { top: [], side: [], ctx: [], legend: [], attach: [], attachOff: 0, panel: 0, state: [] };
const offs = [];
offs.push(wasm.registerWidgetAction({ id: 'w-top', target: 'topbar', label: 'TopAct', icon: 'star', order: 1, run: (ctx) => calls.top.push(ctx.symbol) }));
offs.push(wasm.registerWidgetAction({ id: 'w-side', target: 'sidebar', label: 'SideAct', icon: 'star', run: (ctx) => calls.side.push(ctx.timeframe) }));
offs.push(wasm.registerWidgetAction({ id: 'w-ctx', target: 'context:body', label: 'CtxRow', icon: 'star', order: 100, run: (ctx) => calls.ctx.push(ctx.pointer) }));
offs.push(wasm.registerSidePanel({ id: 'w-panel', title: 'WPanel', icon: 'pen', mount: (body) => { calls.panel += 1; body.textContent = 'wp'; return {}; } }));
offs.push(wasm.registerLegendAction({ id: 'w-legend', icon: 'pen', tooltip: 'LegendAct', run: (ctx, info) => calls.legend.push([ctx.symbol, info.id]) }));
offs.push(wasm.registerSymbolRanking((list) => list.slice().reverse()));
offs.push(wasm.registerWidgetAttachment({ id: 'w-att', mount: (ctx) => { calls.attach.push(ctx.symbol); return () => { calls.attachOff += 1; }; } }));
offs.push(wasm.registerStatePersistence({ key: 'smoke.global', scope: 'global', serialize: (ctx) => ({ symbol: ctx.symbol }), restore: (value) => calls.state.push(['global', value]) }));
offs.push(wasm.registerStatePersistence({ key: 'smoke.cell', scope: 'cell', serialize: (ctx) => ({ cell: ctx.cellId }), restore: (value, ctx) => calls.state.push(['cell', ctx.cellId, value]) }));
offs.push(wasm.registerLocale({ locale: 'tr', messages: { 'widget.sidePanel.close': 'KAPAT-HOST' } }));
check('every register function returns its remover', offs.every((f) => typeof f === 'function'));
check('the shipped locale list is exported', wasm.builtinLocales().length === 20 && wasm.builtinLocales().includes('tr'));

// ═══ 1. the DEFAULT widget: no shell options at all ═══

let host = container(W, H);
let w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA', timeframe: '60', persist: 'vela-smoke-widget' });
current = w;
await w.ready();
await settle();
let root = rootOf(host);
const topbar = w.region('topbar');
const content = topbar.children[0];
check('the default widget has a topbar', topbar.children.length === 1 && hasClass(content, 'vela-topbar-content'));
const left = content.children.filter((c) => !hasClass(c, 'vela-topbar-right') && !hasClass(c, 'vela-sep')).map((c) => String(c.className).split(' ')[0]);
check('single-chart: symbol, timeframes, style, indicators, the contributed left cluster, undo, redo (no layout button)', left.join() === 'vela-widget-symbol,vela-widget-tf-group,vela-widget-style,vela-widget-indicators,vela-widget-actions-left,vela-widget-tool,vela-widget-tool', left.join());
check('the default widget has the bottom bar', w.region('bottombar').children.length === 1 && byClass(w.region('bottombar'), 'vela-bb-settings').length === 1);
check('and a status line and a watermark in its cell', byClass(root, 'vela-statusline').length === 1 && byClass(root, 'vela-watermark').length === 1);
check('and no sidebar (off by default)', w.region('edge').children.length === 0 && !root.hasAttribute('data-vela-sidebar'));
click(one(content, 'vela-widget-tf-caret'));
const presetIds = () => byClass(root, 'vela-st-menu-item').map((i) => i.getAttribute('data-id')).filter((id) => /^(\d+|[DWM])$/.test(id));
check('the default timeframe presets are the original\'s', presetIds().join() === '1,5,15,30,60,240,D,W,M', presetIds().join());
key(doc, 'Escape');
await settle();

// contributed topbar action (ONE registry for the root, the top and the side)
const topAct = one(content, 'vela-widget-action');
check('a contributed topbar action registered from JS is projected', byClass(content, 'vela-widget-action').some((b) => b.getAttribute('data-action-id') === 'w-top'), byClass(content, 'vela-widget-action').map((b) => b.getAttribute('data-action-id')).join());
click(byClass(content, 'vela-widget-action').find((b) => b.getAttribute('data-action-id') === 'w-top'));
check('and runs with the live context (symbol)', calls.top.length === 1 && calls.top[0].endsWith('AAA'), calls.top.join());
void topAct;

// attachments mount once per widget with the context
check('a registered attachment mounted once with the widget context', calls.attach.length === 1 && calls.attach[0].endsWith('AAA'), calls.attach.join());

// panel toggles in the topbar <-> the dock
const panelsHost = one(content, 'vela-widget-panels');
const toggle = (id) => byClass(panelsHost, `vela-widget-panel-${id}`)[0];
const panelOrder = panelsHost.children.map((b) => String(b.className).split('vela-widget-panel-')[1]);
check('the topbar shows the dock\'s panel group (the built-ins, then the contributed panels)', panelOrder.length === 3 && panelOrder.at(-1) === 'w-panel' && panelOrder.includes('objects') && panelOrder.includes('dataWindow'), panelOrder.join());
check('every panel toggle has an accessible name', panelsHost.children.every((b) => b.getAttribute('aria-label')) && toggle('objects').getAttribute('aria-label') === 'Object tree');
const main = w.region('main');
const panelEl = (id) => main.children.find((c) => hasClass(c, 'vela-panel') && c.getAttribute('data-panel-id') === id);
click(toggle('objects'));
check('a topbar toggle opens the dock panel and reflects its state', panelEl('objects') && !panelEl('objects').hidden && toggle('objects').getAttribute('data-active') === '1');
click(toggle('dataWindow'));
check('opening another closes the first (the single-open rule) and the toggles follow', panelEl('objects').hidden && !panelEl('dataWindow').hidden && toggle('dataWindow').getAttribute('data-active') === '1' && !toggle('objects').hasAttribute('data-active'));
click(toggle('dataWindow'));
check('a second press closes it', panelEl('dataWindow').hidden && !toggle('dataWindow').hasAttribute('data-active'));
click(toggle('w-panel'));
check('a contributed panel opens from its topbar toggle and mounted once', calls.panel === 1 && !panelEl('w-panel').hidden);
click(one(panelEl('w-panel'), 'vela-panel-close'));
check('the panel\'s own close releases the topbar toggle', panelEl('w-panel').hidden && !toggle('w-panel').hasAttribute('data-active'));

// the dock reacts to a restored document at once
const saved = w.getState();
const withPanels = JSON.parse(JSON.stringify(saved));
withPanels.panels = { open: 'objects' };
w.applyState(withPanels);
check('applyState with `panels` restores the dock immediately (no debounce)', !panelEl('objects').hidden && toggle('objects').getAttribute('data-active') === '1');
click(toggle('objects'));

// the symbol ranking hook
click(one(content, 'vela-widget-symbol'));
await settle();
key(doc, 'Escape');
await settle();
// The first open shows the raw pool: the ranking lands onto the next repaint.
click(one(content, 'vela-widget-symbol'));
await settle();
const rows = byClass(root, 'vela-sp-row').map((r) => r.getAttribute('data-ticker'));
check('the registered symbol ranking orders the picker\'s pool (its head is the opening screen)', rows.length >= 3 && rows[0] === 'DDD' && rows.at(-1) === 'AAA', rows.join());
key(doc, 'Escape');
await settle();

// the typing router knows about the dialogs of the top
click(one(content, 'vela-widget-indicators'));
await settle();
check('the indicator picker is a dialog', dialogsOf(root).length === 1);
key(root, 'b');
await settle();
check('typing while a dialog is open does not open the symbol search', byClass(root, 'vela-sp-input').length === 0 && dialogsOf(root).length === 1, `${dialogsOf(root).length} dialogs`);
key(doc, 'Escape');
await settle();
check('closing it releases the counter: a bare letter opens the search again', (key(root, 'b'), await settle(), byClass(root, 'vela-sp-input').length === 1));
key(doc, 'Escape');
await settle();
key(root, '4');
await settle();
check('a digit opens the timeframe entry (the router knows it is the timeframe entry)', byClass(root, 'vela-tq-input').length === 1);
key(root, '5');
await settle();
check('typing into the open timeframe entry stays in it (no second dialog)', dialogsOf(root).length === 1 && one(root, 'vela-tq-input').value === '45', one(root, 'vela-tq-input')?.value);
key(doc, 'Escape');
await settle();

// the `alerts` intent of the side opens the topbar's alerts menu
root.dispatch('vela:shell-intent', { detail: { intent: 'alerts', text: '' }, bubbles: true });
await settle();
check('the `alerts` intent opens the alerts menu of the top', byClass(root, 'vela-st-menu').length === 1 && byClass(root, 'vela-st-menu-item').length === 1 && text(byClass(root, 'vela-st-menu-item')[0]) === 'No alerts yet');
key(byClass(root, 'vela-st-menu')[0], 'Escape');
await settle();
check('and Escape closes it', byClass(root, 'vela-st-menu').length === 0);

// legend actions contributed from JS
w.cell().addIndicator('ema', { length: 21 });
await settle();
const legendBtn = root.all((n) => n.getAttribute?.('aria-label') === 'LegendAct' || n.getAttribute?.('title') === 'LegendAct')[0];
check('a registered legend action shows on the indicator\'s legend row', !!legendBtn);
if (legendBtn) click(legendBtn);
check('and runs with the context and the row\'s indicator', calls.legend.length === 1 && calls.legend[0][0].endsWith('AAA') && typeof calls.legend[0][1] === 'string', JSON.stringify(calls.legend));

// the screenshot: the topbar button and the chord, one cell
const shotBtn = one(content, 'vela-widget-screenshot');
downloads.length = 0;
click(shotBtn);
check('the screenshot button downloads a PNG of the one cell', downloads.length === 1 && downloads[0].href.startsWith('data:image/png') && downloads[0].href.includes(`size=`) && downloads[0].name === 'AAA-60.png', JSON.stringify(downloads));
downloads.length = 0;
key(root, 's', { ctrlKey: true, altKey: true });
check('the chord mod+alt+s (registered by the screenshot slot) downloads the same PNG', downloads.length === 1 && downloads[0].href.startsWith('data:image/png') && downloads[0].name === 'AAA-60.png', JSON.stringify(downloads));
check('the widget API screenshot is a PNG data URL', w.screenshot().startsWith('data:image/png'));

// dialogs mount at the root
const gear = one(w.region('bottombar'), 'vela-bb-settings');
click(gear);
await settle();
const settingsDlg = root.all((n) => hasClass(n, 'vela-dlg-positioner'))[0];
check('the chart settings dialog mounts at the widget root (not inside the cell)', !!settingsDlg && settingsDlg.parentNode === root, settingsDlg && String(settingsDlg.parentNode.className));
check('and carries the chrome colors the cell holds', !!settingsDlg && settingsDlg.style.getPropertyValue('--vela-chrome-background') !== '');
if (settingsDlg) key(doc, 'Escape');
await settle();
check('closing removes it', root.all((n) => hasClass(n, 'vela-dlg-positioner')).length === 0);

// ═══ 2. Turkish ═══
const english = labelsOf(root);
w.setLocale('tr');
await settle();
const turkish = labelsOf(root);
check('the shipped Turkish pack is registered on first use: topbar labels', ['Geri al', 'Yinele', 'Alarmlar', 'Ekran görüntüsünü indir'].every((l) => turkish.has(l)) && !turkish.has('Undo'), [...turkish].slice(0, 12).join(' | '));
check('the legend (row buttons)', ['Ayarlar', 'Gizle'].every((l) => turkish.has(l)));
check('the drawing toolbar (tool labels)', turkish.has('Trend çizgisi'));
check('the shell labels (panel toggles)', turkish.has('Nesne ağacı') && turkish.has('Veri penceresi'));
check('the chart style button and the bottom bar', [...turkish].some((l) => l.startsWith('Grafik stili')) && turkish.has('Grafik ayarları'));
click(one(content, 'vela-widget-symbol'));
await settle();
check('the symbol search dialog', text(one(root, 'vela-st-title')) === 'Sembol arama', text(one(root, 'vela-st-title')));
key(doc, 'Escape');
await settle();
click(one(w.region('bottombar'), 'vela-bb-settings'));
await settle();
const trDlg = root.all((n) => hasClass(n, 'vela-dlg-positioner'))[0];
check('the settings dialog (at the root) reads Turkish', !!trDlg && text(one(trDlg, 'vela-dlg-title')) !== 'Settings' && text(one(trDlg, 'vela-dlg-title')) !== '', text(one(trDlg ?? root, 'vela-dlg-title')));
key(doc, 'Escape');
await settle();
click(toggle('objects'));
await settle();
check('the object tree panel (title and rows)', text(one(panelEl('objects'), 'vela-panel-title')) === 'Nesne ağacı' && text(one(panelEl('objects'), 'vela-ot-panename')) === 'Ana grafik', text(one(panelEl('objects'), 'vela-ot-panename')));
check('a host pack registered with registerLocale wins over the shipped one', one(panelEl('objects'), 'vela-panel-close').getAttribute('aria-label') === 'KAPAT-HOST', one(panelEl('objects'), 'vela-panel-close').getAttribute('aria-label'));
click(toggle('objects'));
// a menu
click(one(content, 'vela-widget-tf-caret'));
await settle();
check('the timeframe menu (a menu)', byClass(root, 'vela-st-menu').length === 1);
key(doc, 'Escape');
await settle();
w.setLocale('en');
await settle();
check('and back to English', labelsOf(root).has('Undo') && english.has('Undo'));
w.setLocale('xx-YY');
await settle();
check('a language without a pack falls back to English instead of failing', labelsOf(root).has('Undo'));

// state persistence handlers registered from JS ride in the document
const doc1 = w.getState();
check('a global persistence handler puts its entry in the document', doc1.ext?.['smoke.global']?.symbol?.endsWith('AAA'), JSON.stringify(doc1.ext));
check('a cell persistence handler puts its entry in the cell', doc1.charts?.[0]?.ext?.['smoke.cell']?.cell === doc1.charts[0].id, JSON.stringify(doc1.charts?.[0]?.ext));
const restoreCalls = calls.state.length;
const doc2 = JSON.parse(JSON.stringify(doc1));
doc2.ext['smoke.global'] = { symbol: 'RESTORED' };
w.applyState(doc2);
check('applyState hands the entries back to the handlers', calls.state.slice(restoreCalls).some((c) => c[0] === 'global' && c[1].symbol === 'RESTORED') && calls.state.slice(restoreCalls).some((c) => c[0] === 'cell'), JSON.stringify(calls.state.slice(restoreCalls)));

// the dock is restored at boot from the persisted document
click(toggle('objects'));
await settle();
w.destroy();
await settle();
check('the default widget\'s destroy ran the attachment disposer and emptied the host', calls.attachOff === 1 && host.children.length === 0, `${calls.attachOff}`);

// the persisted document brings the dock back at construction
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA', timeframe: '60', persist: 'vela-smoke-widget' });
current = w;
await w.ready();
await settle();
const bootPanel = w.region('main').children.find((c) => hasClass(c, 'vela-panel') && c.getAttribute('data-panel-id') === 'objects');
const bootToggle = one(w.region('topbar'), 'vela-widget-panel-objects');
check('a restored document opens the dock\'s panel at construction and the topbar toggle shows it', !!bootPanel && !bootPanel.hidden && bootToggle.getAttribute('data-active') === '1');
w.destroy();
await settle();

// the single-chart wrappers take the language as well
{
    const h = container(W, H);
    const chart = new wasm.VelaChart(h, 'dark');
    chart.setBars(bars(1, 50));
    chart.addIndicator('ema', undefined);
    await settle();
    const before = labelsOf(h);
    chart.setLocale('tr');
    await settle();
    const after = labelsOf(h);
    check('VelaChart.setLocale re-reads the legend and the chrome in the pack\'s language', before.has('Hide') && after.has('Gizle'), [...after].slice(0, 8).join(' | '));
    chart.destroy();
    const h2 = container(W, H);
    const app = new wasm.VelaApp(h2, 'dark', 'AAA', '1h', bars(2, 50));
    await settle();
    app.setLocale('tr');
    await settle();
    check('VelaApp.setLocale re-reads the legend and the chrome in the pack\'s language', labelsOf(h2).has('Gizle') || labelsOf(h2).has('Ayarlar'), [...labelsOf(h2)].slice(0, 8).join(' | '));
    app.destroy();
}

// ═══ 3. the shell OPTIONS ═══

host = container(W, H);
w = new wasm.VelaWidget(host, {
    bars: seedBars(),
    symbol: 'AAA',
    topbar: { left: ['symbol', 'timeframes'], right: ['screenshot'] },
    timeframes: ['5', '60', 'D'],
    indicatorPicker: false,
    bottombar: false,
    statusline: false,
    watermark: false,
});
current = w;
await w.ready();
await settle();
root = rootOf(host);
let c2 = w.region('topbar').children[0];
check('`topbar` composes the entries per side', one(c2, 'vela-widget-symbol') && one(c2, 'vela-widget-tf-group') && byClass(c2, 'vela-widget-style').length === 0 && !one(c2, 'vela-widget-undo') && !one(c2, 'vela-widget-alerts') && !one(c2, 'vela-widget-panels') && one(c2, 'vela-widget-screenshot'), c2.children.map((c) => c.className).join(' | '));
check('`indicatorPicker: false` removes the picker button', !one(c2, 'vela-widget-indicators'));
check('`bottombar: false`, `statusline: false`, `watermark: false`', w.region('bottombar').children.length === 0 && byClass(root, 'vela-statusline').length === 0 && byClass(root, 'vela-watermark').length === 0);
click(one(c2, 'vela-widget-tf-caret'));
const tfIds = byClass(root, 'vela-st-menu-item').map((i) => i.getAttribute('data-id')).filter((id) => /^(\d+|[DWM])$/.test(id));
check('`timeframes` sets the presets of the dropdown', tfIds.join() === '5,60,D', tfIds.join());
key(doc, 'Escape');
await settle();
downloads.length = 0;
key(root, 's', { ctrlKey: true, altKey: true });
check('the screenshot chord is there while the composition keeps the screenshot slot', downloads.length === 1, `${downloads.length}`);
w.destroy();
await settle();

// shell defaults: the host's own value always wins
const offDefaults = wasm.registerShellDefaults({ sidebar: true });
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await w.ready();
await settle();
check('`registerShellDefaults({ sidebar })` gives a sidebar to a widget that did not say', w.region('edge').children.length === 1 && rootOf(host).hasAttribute('data-vela-sidebar'));
w.destroy();
await settle();
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA', sidebar: false });
await w.ready();
await settle();
check('and `sidebar: false` of the host wins over it', w.region('edge').children.length === 0);
w.destroy();
await settle();
offDefaults();
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await w.ready();
await settle();
check('the remover takes the default away', w.region('edge').children.length === 0);
w.destroy();
await settle();

// ── shell defaults: the composition form, the readback, a widget built earlier is untouched ──

check('shellDefaults() reads {} when nothing is registered', JSON.stringify(wasm.shellDefaults()) === '{}');
host = container(W, H);
const wEarly = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await wEarly.ready();
await settle();
const hostEarly = host;
const offComp = wasm.registerShellDefaults({ sidebar: { top: ['panels'], bottom: [] } });
check('a composition default reads back as given', JSON.stringify(wasm.shellDefaults()) === JSON.stringify({ sidebar: { top: ['panels'], bottom: [] } }), JSON.stringify(wasm.shellDefaults()));
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await w.ready();
await settle();
check('a composition default gives the widget built afterwards its sidebar, the one built earlier stays without', rootOf(host).hasAttribute('data-vela-sidebar') && !rootOf(hostEarly).hasAttribute('data-vela-sidebar'));
w.destroy();
await settle();
const offTrue = wasm.registerShellDefaults({ sidebar: true });
offComp();
check('a remover takes only its own value away', JSON.stringify(wasm.shellDefaults()) === JSON.stringify({ sidebar: true }), JSON.stringify(wasm.shellDefaults()));
offTrue();
check('and the last one leaves {}', JSON.stringify(wasm.shellDefaults()) === '{}');
wEarly.destroy();
await settle();

// ── keyboard layouts and input methods at the widget root ──

host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await w.ready();
await settle();
root = rootOf(host);
downloads.length = 0;
key(root, 'ы', { code: 'KeyS', ctrlKey: true, altKey: true });
check('the screenshot chord (Ctrl+Alt+S) still works on a Cyrillic layout (physical key)', downloads.length === 1, `${downloads.length}`);
check('Alt+R (reset chart view) is handled on the root, on a Latin and on a Cyrillic layout', key(root, 'r', { code: 'KeyR', altKey: true }).defaultPrevented === true && key(root, 'к', { code: 'KeyR', altKey: true }).defaultPrevented === true);
check('a chord nothing is bound to is left alone', key(root, 'q', { code: 'KeyQ', altKey: true }).defaultPrevented !== true);
const composing = key(root, 'n', { isComposing: true });
check('a key an input method is composing never opens symbol search', composing.defaultPrevented !== true);
const processKey = key(root, 'Process');
check('the IME "Process" key never opens symbol search either', processKey.defaultPrevented !== true);
const typed = key(root, 'n');
check('a plain letter on the root is taken (symbol search opens seeded with it)', typed.defaultPrevented === true);
key(doc, 'Escape');
await settle();
w.destroy();
await settle();

// ── ctx.dockStrip / ctx.dockEdge (workspace-dock-strip) ──

let stripCtx = null;
const offStripAtt = wasm.registerWidgetAttachment({ id: 'test-strip', mount(ctx) { stripCtx = ctx; return () => {}; } });
host = container(W, H);
w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA' });
await w.ready();
await settle();
root = rootOf(host);
const elems = (n) => n.children.filter((c) => !String(c.tagName).startsWith('#'));
const stripsEl = w.region('strips');
const a1 = doc.createElement('div');
const b1 = doc.createElement('div');
const undockA = stripCtx.dockStrip(a1);
const undockB = stripCtx.dockStrip(b1);
const names = root.children.map((c) => String(c.className).split(' ')[0]);
check('a docked strip sits between the chart row and the bottom bar, in docking order', names.indexOf('vela-ws-main') < names.indexOf('vela-ws-strips') && names.indexOf('vela-ws-strips') < names.indexOf('vela-widget-bottombar') && elems(stripsEl).join() === [a1, b1].join() && elems(stripsEl)[0] === a1 && elems(stripsEl)[1] === b1);
check('the docked strip is the undock function, with a cover member', typeof undockA === 'function' && typeof undockA.cover === 'function');
undockA();
check('undock removes only that strip', elems(stripsEl).length === 1 && elems(stripsEl)[0] === b1 && a1.parentNode === null);
const elsewhere = doc.createElement('section');
elsewhere.appendChild(b1);
undockB();
check('undock is a no-op once the owner took the element elsewhere', b1.parentNode === elsewhere && elems(stripsEl).length === 0);
const belowEl = doc.createElement('div');
stripCtx.dockStrip(belowEl, { position: 'below-bottombar' });
check('below-bottombar docks into the strips-below region', belowEl.parentNode === w.region('strips-below') && String(belowEl.parentNode.className).includes('vela-ws-strips-below'));
belowEl.remove();
// cover: the strip becomes a direct child of the root and leaves a placeholder in its home
const a2 = doc.createElement('div');
const b2 = doc.createElement('div');
const stripA2 = stripCtx.dockStrip(a2);
stripCtx.dockStrip(b2);
stripA2.cover(true);
check('covering moves the strip to the root and marks it', a2.parentNode === root && a2.hasAttribute('data-vela-cover') && root.hasAttribute('data-vela-regions'));
check('its home keeps only the other strip (a comment holds the place)', elems(stripsEl).length === 1 && elems(stripsEl)[0] === b2 && stripsEl.children.some((c) => String(c.tagName) === '#COMMENT'));
stripA2.cover(true);
check('covering twice is harmless', a2.parentNode === root && stripsEl.children.filter((c) => String(c.tagName) === '#COMMENT').length === 1);
stripA2.cover(false);
check('uncovering puts it back in its own spot, ahead of the other', elems(stripsEl)[0] === a2 && elems(stripsEl)[1] === b2 && !a2.hasAttribute('data-vela-cover') && !stripsEl.children.some((c) => String(c.tagName) === '#COMMENT'));
stripA2.cover(true);
stripA2();
check('a covered strip undocks too, leaving no placeholder', a2.parentNode === null && elems(stripsEl).length === 1 && elems(stripsEl)[0] === b2 && !stripsEl.children.some((c) => String(c.tagName) === '#COMMENT') && !root.children.includes(a2));
check('the stylesheet carries the covering rule', doc.getElementById('vela-workspace').textContent.includes('[data-vela-cover]'));
const rail = doc.createElement('nav');
const undockRail = stripCtx.dockEdge(rail);
check('dockEdge appends to the edge region', rail.parentNode === w.region('edge') && rail.parentNode.parentNode === root);
undockRail();
check('and its undock removes it', rail.parentNode === null);
const rail2 = doc.createElement('nav');
const undockRail2 = stripCtx.dockEdge(rail2);
const elsewhere2 = doc.createElement('section');
elsewhere2.appendChild(rail2);
undockRail2();
check('the edge undock leaves an element that moved elsewhere alone', rail2.parentNode === elsewhere2);
offStripAtt();
w.destroy();
await settle();

// ── state stays readable while and after tearing down (workspace-destroy-flush) ──

{
    const flushStorage = [];
    host = container(W, H);
    w = new wasm.VelaWidget(host, { bars: seedBars(), symbol: 'AAA', persist: 'vela-smoke-flush' });
    await w.ready();
    await settle();
    let told = 0;
    let seenZone = null;
    let nested = 0;
    w.on('state:changed', () => {
        told += 1;
        nested += 1;
        w.destroy(); // a host that unmounts from its own save handler: no recursion
        try { seenZone = w.getState().timezone ?? null; } catch (_) { seenZone = 'threw'; }
        nested -= 1;
    });
    w.setTimezone('Europe/Paris');
    check('nothing is reported inside the debounce window', told === 0);
    w.destroy();
    check('destroy reports the pending edit once, and a handler that destroys again does not recurse', told === 1 && nested === 0, `${told}`);
    check('getState works from inside that handler and after destroy (what was on screen)', seenZone === 'Europe/Paris' && w.getState().timezone === 'Europe/Paris', String(seenZone));
    check('the storage holds what the host was told', JSON.parse(storage.get('vela-smoke-flush')).timezone === 'Europe/Paris');
    void flushStorage;
    await settle();
}

// ═══ 4. a 2x2 workspace with a sidebar: the side of the shell on the same registry ═══

host = container(W, H);
const ws = new wasm.VelaWorkspace(host, {
    layout: '4',
    bars: seedBars(),
    theme: 'dark',
    sidebar: true,
    persist: 'vela-smoke-widget-ws',
    cells: Object.fromEntries(SYMBOLS.map((s) => [s.toLowerCase(), { symbol: s, timeframe: '60' }])),
});
current = ws;
await ws.ready();
await settle();
root = rootOf(host);
const nav = ws.region('edge').children[0];
const sbtn = (id) => byClass(nav, 'vela-sidebar-btn').find((b) => b.getAttribute('data-id') === id);
check('the sidebar lists the contributed panel and the contributed action from the same registry', !!sbtn('w-panel') && !!sbtn('w-side'), byClass(nav, 'vela-sidebar-btn').map((b) => b.getAttribute('data-id')).join());
click(sbtn('w-side'));
check('a sidebar action runs with the root context', calls.side.length === 1 && calls.side[0] === '60', calls.side.join());
const topbar2 = ws.region('topbar').children[0];
check('the layout button shows on a grid (the topbar knows the workspace)', byClass(topbar2, 'vela-widget-style').length === 2);
check('both halves share the dock: the sidebar and the topbar toggles agree', (() => {
    click(sbtn('objects'));
    const topBtn = one(topbar2, 'vela-widget-panel-objects');
    const same = sbtn('objects').getAttribute('aria-pressed') === 'true' && topBtn.getAttribute('data-active') === '1';
    click(sbtn('objects'));
    return same && !topBtn.hasAttribute('data-active');
})());

// a right click on a cell: the contributed row of the one registry
const aRect = cellEl(ws, 'aaa').rect;
const aData = dataCanvas(cellEl(ws, 'aaa'));
aData.dispatch('pointermove', { clientX: aRect.left + 200, clientY: aRect.top + 100, pointerId: 3, pointerType: 'mouse', button: 0, timeStamp: clock });
pump();
aData.dispatch('contextmenu', { clientX: aRect.left + 200, clientY: aRect.top + 100 });
const menu = root.children.find((c) => hasClass(c, 'vela-smenu'));
const item = menu && byClass(menu, 'vela-smenu-item').find((i) => i.getAttribute('data-vela-item') === 'action:w-ctx');
check('a context-menu row contributed from JS shows in the chart menu', !!item);
if (item) click(item);
check('and runs with the pointer captured at open', calls.ctx.length === 1 && typeof calls.ctx[0]?.time === 'number', JSON.stringify(calls.ctx));

// the screenshot of a 2x2
downloads.length = 0;
click(one(topbar2, 'vela-widget-screenshot'));
check('the screenshot button composites the 2x2 layout', downloads.length === 1 && downloads[0].href.includes(`size=${W}x${H}`) && downloads[0].name === 'vela-layout.png', JSON.stringify(downloads));
check('the workspace API screenshot too', ws.screenshot().includes(`size=${W}x${H}`));
ws.maximizeCell('bbb');
await settle();
check('a maximized cell is one cell: its own pile on a surface of its size', ws.screenshot().startsWith('data:image/png'));
ws.maximizeCell(null);
await settle();

// status prefs change on a cell that is NOT active updates its overlay at once
check('cell:statusPrefs and panels:restored are workspace events of the JS API', (() => {
    const fired = [];
    const off1 = ws.on('cell:statusPrefs', (e) => fired.push(e.id));
    const off2 = ws.on('panels:restored', () => fired.push('panels'));
    ws.applyState(ws.getState());
    off1();
    off2();
    return fired.includes('panels');
})());
ws.setActiveCell('aaa');
await settle();
const dEl = cellEl(ws, 'ddd');
const wmOf = (cell) => one(cell, 'vela-watermark');
const shown = (el) => el.style.getPropertyValue('display') !== 'none';
check('the watermark of a cell starts shown', !!wmOf(dEl) && shown(wmOf(dEl)));
// A restored document turns the watermark of a cell that is NOT active off: the workspace
// raises cell:statusPrefs at once and the overlay re-reads (no wait for state:changed).
const seen = [];
const offPrefs = ws.on('cell:statusPrefs', (e) => seen.push(e.id));
const next = JSON.parse(JSON.stringify(ws.getState()));
next.charts.find((c) => c.id === 'ddd').watermark = false;
ws.applyState(next);
check('the event names the cell', seen.includes('ddd'), seen.join());
check('the overlay of the non-active cell updated at once', !shown(wmOf(dEl)), wmOf(dEl).style.getPropertyValue('display'));
offPrefs();

// ═══ 5. the mobile layout: the topbar's contributed actions reach the more menu ═══
ws.destroy();
await settle();
const hostM = container(420, 800);
const wsM = new wasm.VelaWorkspace(hostM, { layout: '2h', bars: seedBars(), cells: { aaa: { symbol: 'AAA', timeframe: '60' }, bbb: { symbol: 'BBB', timeframe: '60' } } });
current = wsM;
await wsM.ready();
await settle();
const rootM = rootOf(hostM);
click(byClass(wsM.region('mobilebar'), 'vela-mb-item').find((s) => s.getAttribute('data-stop') === 'more'));
const sheet = byClass(rootM, 'vela-sheet')[0];
const mdLabels = sheet ? byClass(sheet, 'vela-md-row').map((r) => text(one(r, 'vela-md-row-label'))) : [];
check('the more menu lists the topbar\'s contributed (menu-placed) actions', mdLabels.includes('TopAct'), mdLabels.join());
const callsBefore = calls.top.length;
const topRow = sheet && sheet.all((n) => text(n) === 'TopAct')[0];
if (topRow) click(topRow.parentNode && hasClass(topRow.parentNode, 'vela-md-row') ? topRow.parentNode : topRow);
check('and a pick runs the action with the root context', calls.top.length === callsBefore + 1, `${calls.top.length} vs ${callsBefore}`);
wsM.destroy();
await settle();

offs.forEach((f) => f());
ws.destroy();
await settle();
check('destroy empties the host, every listener, every observer', host.children.length === 0 && observers.every((o) => !o.connected));
console.log(state.failures === 0 ? 'smoke_widget: all checks passed' : `smoke_widget: ${state.failures} FAILED`);
void storage;
void totalListeners;
process.exit(state.failures === 0 ? 0 : 1);
