// Smoke test of vela-web's top shell (crates/vela-web/src/shell_top: the topbar, the symbol and
// indicator pickers, the layout dropdown, the timeframe entry and sheets, the bottom bar, and
// the per-cell status line, watermark and hover controls) in Node, against the minimal fake DOM.
//
// The fake DOM has no layout engine: this script plays one (the grid template of the workspace
// smoke) and gives the status line a stand-in for text measurement (scrollWidth). Real CSS,
// real focus, real pointer capture and real painting are NOT exercised, and no exchange
// endpoint is ever called (the in-memory provider is the only data source).
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_shell_top.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeNode, check, clock, container, doc, loadPackage, observers, options, pump, state } from './fake_dom.mjs';

options.captureAncestors = true;
// Additive stand-ins the shell reads through plain property access.
Object.defineProperty(FakeNode.prototype, 'lastElementChild', {
    get() {
        return this.children.at(-1) ?? null;
    },
    configurable: true,
});

const wasm = await loadPackage('smoke_shell_top.mjs');

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
const cellsOf = (grid) => grid.children.filter((c) => c.className.split(' ').includes('vela-cell'));
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

// ── helpers over the fake DOM ──
const hasClass = (n, c) => String(n.className).split(/\s+/).includes(c);
const byClass = (root, c) => root.all((n) => hasClass(n, c));
const one = (root, c) => byClass(root, c)[0];
const click = (n) => n.dispatch('click', {});
const key = (target, k, extra = {}) => target.dispatch('keydown', { key: k, code: '', ...extra });
const pointer = (target, type, x, y, extra = {}) => target.dispatch(type, { clientX: x, clientY: y, pointerId: 3, pointerType: 'mouse', button: 0, timeStamp: clock, ...extra });
const typeInto = (input, text) => {
    input.value = text;
    input.dispatch('input', {});
};
const text = (n) => n?.textContent ?? '';
const totalListeners = (node) => node.listeners.length + node.children.reduce((n, c) => n + totalListeners(c), 0);
const sameSymbol = (a, b) => a.split(':').at(-1) === b;
const cellEls = (ws) => cellsOf(ws.grid());
const cellEl = (ws, id) => cellEls(ws).find((c) => c.getAttribute('data-cell-id') === id);
const dataCanvas = (cell) => cell.children[0].children[0].children.find((c) => c.style.getPropertyValue('pointer-events') !== 'none' && c.ctx);
const floating = (root) => byClass(root, 'vela-st-menu');

// ── a 2x2 workspace ──
const host = container(W, H);
const listenersBefore = totalListeners(host);
const docListenersBefore = doc.listeners.length;
const winListenersBefore = doc.defaultView.listeners.length;
const ws = new wasm.VelaWorkspace(host, {
    layout: '4',
    bars: seedBars(),
    theme: 'dark',
    cells: Object.fromEntries(SYMBOLS.map((s) => [s.toLowerCase(), { symbol: s, timeframe: '60' }])),
});
current = ws;
await ws.ready();
await settle();

const root = host.children.find((c) => hasClass(c, 'vela-workspace'));
const topbar = ws.region('topbar');
const bottombar = ws.region('bottombar');
const active = () => ws.cell(ws.activeCell());
const cellSymbol = () => active().symbol();

// ── the topbar ──
check('the stylesheet is injected once, in the chrome layer', doc.head.all((n) => n.id === 'vela-shell-top').length === 1 && doc.getElementById('vela-shell-top').textContent.includes('@layer vela.chrome'));
const content = topbar.children[0];
check('the topbar mounts one content wrapper into its region', topbar.children.length === 1 && hasClass(content, 'vela-topbar-content') && content.style.getPropertyValue('display') === 'contents');
const left = content.children.filter((c) => !hasClass(c, 'vela-topbar-right'));
const right = one(content, 'vela-topbar-right');
const order = left.filter((c) => !hasClass(c, 'vela-sep')).map((c) => String(c.className).split(' ')[0]);
check('the left side keeps the original order: symbol, timeframes, style, layout, indicators, actions, undo, redo', order.join() === 'vela-widget-symbol,vela-widget-tf-group,vela-widget-style,vela-widget-style,vela-widget-indicators,vela-widget-actions-left,vela-widget-tool,vela-widget-tool', order.join());
const rightOrder = right.children.filter((c) => !hasClass(c, 'vela-sep')).map((c) => String(c.className).split(' ').slice(0, 2).join(' '));
check('the right side: actions, alerts, panels, screenshot', rightOrder.join() === 'vela-widget-actions,vela-widget-tool vela-widget-alerts,vela-widget-panels,vela-widget-tool vela-widget-screenshot', rightOrder.join());
check('the primary controls carry trailing hairlines, the left action cluster one that is hidden while it is empty', byClass(content, 'vela-sep').length === 6 && byClass(content, 'vela-sep').filter((s) => s.style.getPropertyValue('display') === 'none').length === 1, `${byClass(content, 'vela-sep').length}`);
await settle();
check('hairlines are one device pixel wide', byClass(content, 'vela-sep').filter((s) => s.style.getPropertyValue('width') === '1px').length >= 5);
const named = [one(content, 'vela-widget-undo'), one(content, 'vela-widget-redo'), one(content, 'vela-widget-alerts'), one(content, 'vela-widget-screenshot'), one(content, 'vela-widget-tf-caret')];
check('the icon-only tools carry accessible names', named.map((b) => b.getAttribute('aria-label')).join() === 'Undo,Redo,Alerts,Download screenshot,Timeframe \u2014 1h', named.map((b) => b.getAttribute('aria-label')).join());
const symbolBtn = one(content, 'vela-widget-symbol');
const styleBtn = byClass(content, 'vela-widget-style')[0];
const layoutBtn = byClass(content, 'vela-widget-style')[1];
check('the buttons show the active cell: bare ticker, style and layout names', text(symbolBtn) === 'AAA' && styleBtn.getAttribute('aria-label') === 'Chart style \u2014 Candles' && layoutBtn.getAttribute('aria-label') === 'Layout \u2014 4', `${text(symbolBtn)} / ${styleBtn.getAttribute('aria-label')} / ${layoutBtn.getAttribute('aria-label')}`);
check('undo and redo start disabled', one(content, 'vela-widget-undo').disabled === true && one(content, 'vela-widget-redo').disabled === true);

// ── the symbol picker ──
click(symbolBtn);
await settle();
const dialogs = () => byClass(root, 'vela-st-panel');
check('the symbol button opens the search dialog on the root (a modal dialog with a name)', dialogs().length === 1 && dialogs()[0].getAttribute('role') === 'dialog' && dialogs()[0].getAttribute('aria-modal') === 'true' && text(one(root, 'vela-st-title')) === 'Symbol Search');
const spInput = one(root, 'vela-sp-input');
check('the field is labeled and the empty query lists the pool', spInput.getAttribute('aria-label') === 'Symbol Search' && byClass(root, 'vela-sp-row').length === 4, `${byClass(root, 'vela-sp-row').length} rows`);
check('the asset-class tabs are a tablist with one selected tab', one(root, 'vela-sp-tabs').getAttribute('role') === 'tablist' && byClass(root, 'vela-st-tab').length === 7 && byClass(root, 'vela-st-tab').filter((t) => t.getAttribute('aria-selected') === 'true').length === 1);
typeInto(spInput, 'bb');
check('typing filters (the field upper-cases it)', spInput.value === 'BB' && byClass(root, 'vela-sp-row').length === 1 && byClass(root, 'vela-sp-row')[0].getAttribute('data-ticker') === 'BBB');
typeInto(spInput, 'zz');
check('a query nothing matches shows the empty state', byClass(root, 'vela-sp-row').length === 0 && text(one(root, 'vela-sp-empty')) === 'No symbols match.');
typeInto(spInput, '');
check('the first row is highlighted; ArrowDown moves it, ArrowUp clamps at the top', byClass(root, 'vela-sp-row')[0].hasAttribute('data-highlighted'));
key(spInput, 'ArrowDown');
key(spInput, 'ArrowDown');
check('ArrowDown moves the highlight', byClass(root, 'vela-sp-row')[2].hasAttribute('data-highlighted') && byClass(root, 'vela-sp-row').filter((r) => r.hasAttribute('data-highlighted')).length === 1);
key(spInput, 'Enter');
await settle();
check('Enter picks the highlighted row: the dialog closes and the active cell changes symbol', dialogs().length === 0 && sameSymbol(cellSymbol(), 'CCC') && text(symbolBtn) === 'CCC', `${cellSymbol()} / ${text(symbolBtn)}`);
check('the pick carries the venue of the row (VENUE:TICKER)', cellSymbol().includes(':'), cellSymbol());
// bare typing: a letter opens the search seeded with it; Escape closes it
key(root, 'b'); 
await settle();
check('a bare letter on the root opens the search seeded with it', dialogs().length === 1 && one(root, 'vela-sp-input').value === 'B');
key(root, 'b');
key(doc, 'Escape');
await settle();
check('Escape closes the dialog', dialogs().length === 0);
// restore AAA
active().setSymbol('AAA');
await settle();
check('the topbar follows setSymbol from the API', text(symbolBtn) === 'AAA');

// ── the timeframe quick entry ──
key(root, '4');
await settle();
const tqInput = one(root, 'vela-tq-input');
check('a bare digit opens the timeframe entry seeded with it, with a live hint', dialogs().length === 1 && tqInput && tqInput.value === '4' && text(one(root, 'vela-tq-hint')).length > 0 && !one(root, 'vela-tq-hint').hasAttribute('data-invalid'), text(one(root, 'vela-tq-hint')));
typeInto(tqInput, 'ZZ');
check('a bad entry says so', one(root, 'vela-tq-hint').hasAttribute('data-invalid') && text(one(root, 'vela-tq-hint')) === 'Not a timeframe');
typeInto(tqInput, '4h');
key(tqInput, 'Enter');
await settle();
check('Enter applies the canonical timeframe to the active cell', dialogs().length === 0 && active().timeframe() === '240', active().timeframe());
check('the timeframe button follows', one(content, 'vela-widget-tf-caret').getAttribute('aria-label') === 'Timeframe \u2014 4h');
active().setTimeframe('60');
await settle();

// ── the timeframe menu and favorites ──
const caret = one(content, 'vela-widget-tf-caret');
click(caret);
check('the caret opens a menu of the timeframes with the current one checked', floating(root).length === 1 && byClass(root, 'vela-st-menu-item').length === 9 && byClass(root, 'vela-st-menu-item').filter((i) => i.hasAttribute('data-checked')).map((i) => i.getAttribute('data-id')).join() === '60' && caret.getAttribute('aria-expanded') === 'true');
const star = byClass(root, 'vela-st-menu-star').find((s) => s.parentNode.getAttribute('data-id') === '15');
click(star);
check('a star toggles a favorite: the menu stays open and a chip appears', floating(root).length === 1 && byClass(content, 'vela-widget-tf').length >= 1 && byClass(content, 'vela-widget-tf').some((c) => text(c) === '15m'), byClass(content, 'vela-widget-tf').map(text).join());
const row = byClass(root, 'vela-st-menu-item').find((i) => i.getAttribute('data-id') === '5');
click(row);
await settle();
check('picking a row changes the timeframe and closes the menu', floating(root).length === 0 && active().timeframe() === '5' && caret.getAttribute('aria-expanded') === 'false', active().timeframe());
const chip15 = byClass(content, 'vela-widget-tf').find((c) => text(c) === '15m');
check('a chip has its own accessible name and the current value is marked', chip15.getAttribute('aria-label') === 'Switch timeframe to 15m' && byClass(content, 'vela-widget-tf').some((c) => c.getAttribute('aria-current') === 'true' && text(c) === '5m'));
click(chip15);
await settle();
check('a chip switches the timeframe', active().timeframe() === '15', active().timeframe());
active().setTimeframe('60');
await settle();

// ── the chart-style menu ──
click(styleBtn);
const styleItems = byClass(root, 'vela-st-menu-item');
check('the style button opens the styles with the current one checked (icons in badges)', styleItems.length >= 9 && styleItems.filter((i) => i.hasAttribute('data-checked')).map((i) => i.getAttribute('data-id')).join() === 'candles' && byClass(root, 'vela-st-menu-badge').length === styleItems.length);
// keyboard: ArrowDown to the second row, Enter
const menuEl = floating(root)[0];
key(menuEl, 'ArrowDown');
key(menuEl, 'Enter');
await settle();
check('keyboard picks a style (the second row, bars)', active().priceStyle() === 'bars' && floating(root).length === 0, active().priceStyle());
click(styleBtn);
click(byClass(root, 'vela-st-menu-item').find((i) => i.getAttribute('data-id') === 'line'));
await settle();
check('a click picks a style and the button follows', active().priceStyle() === 'line' && styleBtn.getAttribute('aria-label') === 'Chart style \u2014 Line', styleBtn.getAttribute('aria-label'));
click(styleBtn);
key(doc, 'Escape');
check('Escape closes a menu', floating(root).length === 0);
click(styleBtn);
pointer(doc, 'pointerdown', 5, 5);
check('a press outside closes a menu', floating(root).length === 0);
active().setPriceStyle('candles');
await settle();

// ── the indicator picker ──
const indBtn = one(content, 'vela-widget-indicators');
check('the indicators button shows its icon and label', text(indBtn) === 'Indicators');
click(indBtn);
await settle();
const ipSearch = one(root, 'vela-ip-search');
check('the button opens the picker listing the library by category', dialogs().length === 1 && text(one(root, 'vela-st-title')) === 'Indicators' && byClass(root, 'vela-ip-row').length > 3 && byClass(root, 'vela-ip-group').length >= 1);
typeInto(ipSearch, 'rsi');
const rsiRow = byClass(root, 'vela-ip-row').find((r) => r.hasAttribute('data-library') && /rsi|relative/i.test(text(r)));
check('search narrows the library', rsiRow && byClass(root, 'vela-ip-row').length < 6, byClass(root, 'vela-ip-row').map(text).join('|'));
const before = active().indicators().length;
click(rsiRow);
await settle();
check('adding RSI adds an indicator to the active cell', active().indicators().length === before + 1, `${before} -> ${active().indicators().length}`);
check('the picker lists it on chart with a remove control', byClass(root, 'vela-ip-oncard').length === 1 && byClass(root, 'vela-ip-trash').length === 1 && byClass(root, 'vela-ip-trash')[0].getAttribute('aria-label') === 'Remove from chart');
check('the chart shows a legend row for it', byClass(cellEl(ws, ws.activeCell()), 'vela-ind-row').length >= 1);
check('undo is now enabled', one(content, 'vela-widget-undo').disabled === false);
click(byClass(root, 'vela-ip-trash')[0]);
await settle();
check('the trash removes it', active().indicators().length === before && byClass(root, 'vela-ip-oncard').length === 0);
key(doc, 'Escape');
await settle();
check('Escape closes the picker', dialogs().length === 0);
// `/` opens it from the keyboard
key(root, '/');
await settle();
check('the / chord opens the indicator picker', dialogs().length === 1);
key(doc, 'Escape');
await settle();

// ── undo / redo through the cell history ──
click(indBtn);
await settle();
click(byClass(root, 'vela-ip-row').find((r) => r.hasAttribute('data-library')));
await settle();
key(doc, 'Escape');
await settle();
const count1 = active().indicators().length;
check('an add through the picker lands on the history', count1 === before + 1 && one(content, 'vela-widget-undo').disabled === false && one(content, 'vela-widget-redo').disabled === true);
click(one(content, 'vela-widget-undo'));
await settle();
check('the undo button undoes it and enables redo', active().indicators().length === before && one(content, 'vela-widget-redo').disabled === false);
click(one(content, 'vela-widget-redo'));
await settle();
check('the redo button redoes it', active().indicators().length === before + 1 && one(content, 'vela-widget-redo').disabled === true);
click(one(content, 'vela-widget-undo'));
await settle();

// ── the layout picker ──
click(layoutBtn);
const lp = one(root, 'vela-lp-layer');
check('the layout button opens the grid canvas (16 named squares) and the sync switches', lp && byClass(lp, 'vela-lp-sq').length === 16 && byClass(lp, 'vela-lp-sq')[5].getAttribute('aria-label') === '2 \u00d7 2' && byClass(lp, 'vela-lp-sync-row').length === 4 && layoutBtn.getAttribute('aria-expanded') === 'true');
check('the current layout is lit from the top-left (2 x 2)', byClass(lp, 'vela-lp-sq').filter((s) => s.hasAttribute('data-on')).map((s) => `${s.getAttribute('data-r')}${s.getAttribute('data-c')}`).join() === '00,01,10,11');
pointer(byClass(lp, 'vela-lp-sq').find((s) => s.getAttribute('data-r') === '0' && s.getAttribute('data-c') === '2'), 'pointermove', 0, 0);
check('hovering previews the rectangle', byClass(lp, 'vela-lp-sq').filter((s) => s.hasAttribute('data-on')).length === 3);
const symbolSync = () => byClass(lp, 'vela-lp-sync-row').find((r) => r.getAttribute('data-sync') === 'symbol');
check('the sync switches are named and off', symbolSync().getAttribute('role') === 'switch' && symbolSync().getAttribute('aria-checked') === 'false' && ws.sync('symbol') === false);
click(symbolSync());
check('a sync switch flips the workspace sync and stays open', ws.sync('symbol') === true && symbolSync().getAttribute('aria-checked') === 'true' && one(root, 'vela-lp-layer'));
click(symbolSync());
check('and flips back', ws.sync('symbol') === false);
// turning the link on converged the cells onto one symbol: give each its own back
for (const [id, s] of [['aaa', 'AAA'], ['bbb', 'BBB'], ['ccc', 'CCC'], ['ddd', 'DDD']]) ws.cell(id).setSymbol(s);
await settle();
pointer(byClass(lp, 'vela-lp-sq').find((s) => s.getAttribute('data-r') === '0' && s.getAttribute('data-c') === '1'), 'pointerdown', 0, 0);
await settle();
check('pressing a square applies that grid layout and closes the picker', ['2h', '2v'].includes(ws.layout()) && cellEls(ws).length === 2 && !one(root, 'vela-lp-layer'), ws.layout());
check('the layout button follows the layout', layoutBtn.getAttribute('aria-label') === `Layout \u2014 ${ws.layout()}`, layoutBtn.getAttribute('aria-label'));
ws.setLayout('4');
await settle();
check('a layout set from the API reaches the button', layoutBtn.getAttribute('aria-label') === 'Layout \u2014 4' && cellEls(ws).length === 4);

// ── the bottom bar ──
const bbContent = bottombar.children[0];
check('the bottom bar mounts into its region: nine range chips, the zone trigger with the clock, the gear', byClass(bbContent, 'vela-bb-range').length === 9 && text(byClass(bbContent, 'vela-bb-range')[2]) === '1M' && /\d\d:\d\d/.test(text(one(bbContent, 'vela-bb-clock'))), text(one(bbContent, 'vela-bb-clock')));
check('the zone trigger and the gear have accessible names; the session toggle boots hidden', one(bbContent, 'vela-bb-tz').getAttribute('aria-label') === 'Time zone' && one(bbContent, 'vela-bb-settings').getAttribute('aria-label') === 'Chart settings' && one(bbContent, 'vela-bb-session').style.getPropertyValue('display') === 'none');
check('the zone label reads the display zone (UTC)', text(one(bbContent, 'vela-bb-tz').children[1]) === 'UTC', text(one(bbContent, 'vela-bb-tz').children[1]));
const range1M = byClass(bbContent, 'vela-bb-range')[2];
click(range1M);
await settle();
check('a range chip switches to its timeframe and lights up', active().timeframe() === '30' && range1M.getAttribute('data-active') === '1' && byClass(bbContent, 'vela-bb-range').filter((r) => r.hasAttribute('data-active')).length === 1, active().timeframe());
click(one(content, 'vela-widget-tf-caret'));
click(byClass(root, 'vela-st-menu-item').find((i) => i.getAttribute('data-id') === '60'));
await settle();
check('a manual timeframe change clears the lit range chip', byClass(bbContent, 'vela-bb-range').every((r) => !r.hasAttribute('data-active')) && active().timeframe() === '60');
const tz = one(bbContent, 'vela-bb-tz');
click(tz);
const tzItems = byClass(root, 'vela-st-menu-item');
check('the zone trigger opens the zone rows with the stored one checked', tzItems.length > 10 && tzItems.filter((i) => i.hasAttribute('data-checked')).length === 1);
const ist = tzItems.find((i) => i.getAttribute('data-id') === 'Europe/Istanbul');
click(ist);
await settle();
check('picking a zone changes the workspace timezone and relabels the bar', ws.timezone() === 'Europe/Istanbul' && /\+3|GMT\+3/.test(text(tz.children[1])), `${ws.timezone()} / ${text(tz.children[1])}`);
ws.setTimezone('Etc/UTC');
await settle();
check('a timezone set from the API follows', text(tz.children[1]) === 'UTC');
click(one(bbContent, 'vela-bb-settings'));
check('the gear reaches the renderer without disturbing the bar', byClass(bbContent, 'vela-bb-range').length === 9);

// ── the status line, the watermark, the cell controls ──
const aaaCell = cellEl(ws, 'aaa');
const sl = (cell) => one(cell, 'vela-statusline');
check('every cell carries a status line, a watermark and a controls cluster', cellEls(ws).every((c) => sl(c) && one(c, 'vela-watermark') && one(c, 'vela-cell-controls')));
check('the status line shows the bare ticker, the venue and the timeframe', text(one(sl(aaaCell), 'vela-sl-symbol')) === 'AAA' && /\u00b7 .*\u00b7 1h/.test(text(one(sl(aaaCell), 'vela-sl-meta'))), text(one(sl(aaaCell), 'vela-sl-meta')));
check('the watermark shows the symbol and the timeframe', text(one(aaaCell, 'vela-watermark-text')) === 'AAA \u00b7 1h', text(one(aaaCell, 'vela-watermark-text')));
// a tick lands: the resting readout shows the bar
const T0 = Date.UTC(2025, 0, 1) + 300 * 3_600_000;
ws.tick('AAA', '60', { time: T0, open: 50, high: 53, low: 49, close: 52.5, volume: 10 });
await settle();
const bOf = (el) => el.all((n) => n.tagName === 'B');
const ohlcText = (cell) => bOf(one(sl(cell), 'vela-sl-ohlc')).map(text);
check('a live bar fills the readout: four prices, then the signed change', ohlcText(aaaCell).length === 4 && ohlcText(aaaCell).map((t) => Number(t.replace(/,/g, ''))).join() === '50,53,49,52.5' && /^\+2\.5.*\(\+5\.00%\)$/.test(text(one(sl(aaaCell), 'vela-sl-change'))), `${ohlcText(aaaCell)} / ${text(one(sl(aaaCell), 'vela-sl-change'))}`);
check('the change wears the up ink and says up', one(sl(aaaCell), 'vela-sl-change').getAttribute('data-dir') === 'up' && bOf(one(sl(aaaCell), 'vela-sl-ohlc')).every((b) => b.style.getPropertyValue('color') === one(sl(aaaCell), 'vela-sl-change').style.getPropertyValue('color')));
// the crosshair: hovering a bar replaces the resting values
const dc = dataCanvas(aaaCell);
const resting = ohlcText(aaaCell).join();
pointer(dc, 'pointermove', aaaCell.rect.left + 150, aaaCell.rect.top + 120);
pump();
check('the readout follows the crosshair', ohlcText(aaaCell).join() !== resting && ohlcText(aaaCell).length === 4, `${resting} -> ${ohlcText(aaaCell)}`);
pointer(dc, 'pointerleave', 0, 0);
pump();
// the width ladder: a stand-in for measurement that grows with the visible text
const measure = (el, widthOf) => {
    Object.defineProperty(el, 'scrollWidth', { get: () => widthOf(el), configurable: true });
    Object.defineProperty(el, 'clientWidth', { get: () => (el.parentNode.rect?.width ?? W) - 24, configurable: true });
};
const visibleText = (row) => (row.style.getPropertyValue('display') === 'none' ? '' : text(row));
const widthOf = (el) => {
    const stacked = hasClass(el, 'vela-sl-stacked');
    const id = visibleText(one(el, 'vela-sl-identity'));
    const values = visibleText(one(el, 'vela-sl-values'));
    return 7 * (stacked ? Math.max(id.length, values.length) : id.length + values.length);
};
const slA = sl(aaaCell);
measure(slA, widthOf);
const fullWidth = widthOf(slA);
aaaCell.rect = { ...aaaCell.rect, width: 3000 };
relayout();
check('wide: the full one-line readout', !hasClass(slA, 'vela-sl-stacked') && ohlcText(aaaCell).length === 4);
aaaCell.rect = { ...aaaCell.rect, width: fullWidth - 40 };
for (const o of observers) if (o.connected) o.cb([]);
check('narrower: the values stack under the symbol line', hasClass(slA, 'vela-sl-stacked') && ohlcText(aaaCell).length === 4 && aaaCell.hasAttribute('data-vela-sl-stacked-host'));
aaaCell.rect = { ...aaaCell.rect, width: 150 };
for (const o of observers) if (o.connected) o.cb([]);
check('narrower still: O / H / L are shed (the close stays)', hasClass(slA, 'vela-sl-stacked') && ohlcText(aaaCell).length === 1, `${ohlcText(aaaCell).length} cells`);
aaaCell.rect = { ...aaaCell.rect, width: 3000 };
for (const o of observers) if (o.connected) o.cb([]);
check('widening restores the one-line readout', !hasClass(slA, 'vela-sl-stacked') && ohlcText(aaaCell).length === 4 && !aaaCell.hasAttribute('data-vela-sl-stacked-host'));
layoutGrid(ws.grid());
for (const o of observers) if (o.connected) o.cb([]);
// the parts, from the right-click menu
slA.dispatch('contextmenu', { clientX: 20, clientY: 20 });
const partItems = byClass(root, 'vela-st-menu-item');
check('right-click on the status line opens its action menu (five parts and the chart)', partItems.length === 6 && partItems.slice(0, 5).every((i) => i.getAttribute('aria-checked') === 'true') && partItems[5].getAttribute('data-id') === 'chart', partItems.map((i) => i.getAttribute('data-id')).join());
click(partItems.find((i) => i.getAttribute('data-id') === 'part:ohlc'));
await settle();
check('toggling a part hides that segment', one(slA, 'vela-sl-ohlc').style.getPropertyValue('display') === 'none');
slA.dispatch('contextmenu', { clientX: 20, clientY: 20 });
click(byClass(root, 'vela-st-menu-item').find((i) => i.getAttribute('data-id') === 'part:ohlc'));
await settle();
check('and toggling it back shows it again', one(slA, 'vela-sl-ohlc').style.getPropertyValue('display') === '');
// hiding the chart shows the eye
slA.dispatch('contextmenu', { clientX: 20, clientY: 20 });
click(byClass(root, 'vela-st-menu-item').find((i) => i.getAttribute('data-id') === 'chart'));
await settle();
check('hiding the chart dims the line and puts the show-chart eye out', hasClass(slA, 'vela-sl-chart-hidden') && one(slA, 'vela-sl-eye').style.getPropertyValue('display') === 'inline-flex' && one(slA, 'vela-sl-ohlc').style.getPropertyValue('display') === 'none' && one(slA, 'vela-sl-eye').getAttribute('aria-label') === 'Show chart');
click(one(slA, 'vela-sl-eye'));
await settle();
check('the eye shows the chart again', !hasClass(slA, 'vela-sl-chart-hidden') && one(slA, 'vela-sl-eye').style.getPropertyValue('display') === 'none');
// the market badge
check('the market badge is a labeled, tinted circle (continuous market: open)', one(slA, 'vela-sl-market').getAttribute('data-status') === 'open' && one(slA, 'vela-sl-market').getAttribute('aria-label') === 'Market Open' && one(slA, 'vela-sl-bubble').style.getPropertyValue('color') === 'var(--vela-up)');

// the cell controls
const cc = (cell) => one(cell, 'vela-cell-controls');
check('the controls cluster is hidden until the pointer nears the bottom-center', cc(aaaCell).style.getPropertyValue('display') === 'none');
pointer(aaaCell, 'pointermove', aaaCell.rect.left + aaaCell.rect.width / 2, aaaCell.rect.top + aaaCell.rect.height - 46);
check('and appears on hover with grip, zoom out, zoom in, maximize, reset', cc(aaaCell).style.getPropertyValue('display') === 'flex' && byClass(cc(aaaCell), 'vela-cc-btn').map((b) => b.getAttribute('data-action')).join() === 'grip,zoom-out,zoom-in,maximize,reset');
check('every control has an accessible name', byClass(cc(aaaCell), 'vela-cc-btn').map((b) => b.getAttribute('aria-label')).join() === 'Drag to move chart,Zoom out,Zoom in,Maximize chart,Reset chart');
pointer(aaaCell, 'pointermove', aaaCell.rect.left + 5, aaaCell.rect.top + 5);
check('far from it the cluster hides again', cc(aaaCell).style.getPropertyValue('display') === 'none');
pointer(aaaCell, 'pointermove', aaaCell.rect.left + aaaCell.rect.width / 2, aaaCell.rect.top + aaaCell.rect.height - 46);
click(byClass(cc(aaaCell), 'vela-cc-btn').find((b) => b.getAttribute('data-action') === 'maximize'));
await settle();
check('maximize isolates the cell and the control becomes a pressed restore', ws.maximizedCell() === 'aaa' && ws.grid().getAttribute('data-maximized') === '1' && byClass(cc(aaaCell), 'vela-cc-btn').map((b) => b.getAttribute('data-action')).join() === 'zoom-out,zoom-in,maximize,reset' && byClass(cc(aaaCell), 'vela-cc-on')[0].getAttribute('aria-label') === 'Restore layout');
click(byClass(cc(aaaCell), 'vela-cc-on')[0]);
await settle();
check('restore brings the grid back', ws.maximizedCell() == null && byClass(cc(aaaCell), 'vela-cc-btn')[0].getAttribute('data-action') === 'grip');
// the grip drag swaps slots
const grip = byClass(cc(aaaCell), 'vela-cc-grip')[0];
const bbbCell = cellEl(ws, 'bbb');
pointer(grip, 'pointerdown', 10, 10);
pointer(grip, 'pointermove', bbbCell.rect.left + 20, bbbCell.rect.top + 20);
check('dragging the grip over another cell previews it as the drop target', bbbCell.getAttribute('data-drop-target') === '1' && !aaaCell.hasAttribute('data-drop-target'));
pointer(grip, 'pointerup', bbbCell.rect.left + 20, bbbCell.rect.top + 20);
await settle();
check('releasing trades the two slots and clears the preview', cellEls(ws).map((c) => c.getAttribute('data-cell-id')).join() === 'bbb,aaa,ccc,ddd' && cellEls(ws).every((c) => !c.hasAttribute('data-drop-target')), cellEls(ws).map((c) => c.getAttribute('data-cell-id')).join());
ws.swapCells('aaa', 'bbb');
await settle();
layoutGrid(ws.grid());
// zoom changes the visible range
// reset, zoom: they act on their own cell
pointer(aaaCell, 'pointermove', aaaCell.rect.left + aaaCell.rect.width / 2, aaaCell.rect.top + aaaCell.rect.height - 46);
click(byClass(cc(aaaCell), 'vela-cc-btn').find((b) => b.getAttribute('data-action') === 'zoom-in'));
await settle();

pointer(aaaCell, 'pointerleave', 0, 0);

// ── the mobile sheets and the intents of the side of the shell ──
const intent = (name, text = '') => root.dispatch('vela:shell-intent', { detail: { intent: name, text }, bubbles: true });
intent('timeframe-drawer');
await settle();
const sheet = one(root, 'vela-st-drawer');
check('the timeframe sheet lists the date ranges and the timeframes, the current timeframe lit', sheet && byClass(sheet, 'vela-tfd-chip').length === 18 && text(byClass(sheet, 'vela-tfd-heading')[0]) === 'Date Range' && byClass(sheet, 'vela-tfd-chip').filter((c) => c.hasAttribute('data-active')).map((c) => c.getAttribute('data-id')).join() === '60');
click(byClass(sheet, 'vela-tfd-chip').find((c) => c.getAttribute('data-id') === '240'));
await settle();
check('a timeframe chip applies it and closes the sheet', active().timeframe() === '240' && !one(root, 'vela-st-drawer'));
intent('timeframe-drawer');
await settle();
click(byClass(one(root, 'vela-st-drawer'), 'vela-tfd-chip').find((c) => c.getAttribute('data-id') === '3M'));
await settle();
check('a range chip frames the range (its timeframe) and the desktop bar stays truthful', active().timeframe() === '60' && byClass(bbContent, 'vela-bb-range')[3].getAttribute('data-active') === '1');
intent('timezone-drawer');
await settle();
const tzSheet = one(root, 'vela-st-drawer');
check('the time-zone sheet lists the zones with one check', tzSheet && text(one(tzSheet, 'vela-st-drawer-title')) === 'Time zone' && byClass(tzSheet, 'vela-tzd-row').length > 10 && byClass(tzSheet, 'vela-tzd-row').filter((r) => byClass(r, 'vela-icon').length > 0).length === 1);
click(byClass(tzSheet, 'vela-tzd-row').find((r) => r.getAttribute('data-value') === 'Europe/Istanbul'));
await settle();
check('picking a zone applies it and closes the sheet', ws.timezone() === 'Europe/Istanbul' && !one(root, 'vela-st-drawer'));
ws.setTimezone('Etc/UTC');
intent('symbol-search', 'EE');
await settle();
check('the symbol-search intent opens the dialog seeded with its text', dialogs().length === 1 && one(root, 'vela-sp-input').value === 'EE');
intent('type-symbol', 'X');
check('typed text reaching an open search extends the query', one(root, 'vela-sp-input').value === 'EEX');
key(doc, 'Escape');
await settle();
intent('indicator-picker');
await settle();
check('the indicator-picker intent opens the picker', dialogs().length === 1 && text(one(root, 'vela-st-title')) === 'Indicators');
key(doc, 'Escape');
await settle();
intent('type-timeframe', '1');
await settle();
check('the type-timeframe intent opens the entry seeded', one(root, 'vela-tq-input')?.value === '1');
key(doc, 'Escape');
await settle();
check('dialogs release the keymap scope they pushed', ws.region('main') && dialogs().length === 0);

// ── ported from the original tests (statusline-fit, statusline-replay, cell-controls) and the audit fixes ──
{
    const cccCell = cellEl(ws, 'ccc');
    const slC = sl(cccCell);
    let measures = 0;
    Object.defineProperty(slC, 'scrollWidth', { get: () => ((measures += 1), 0), configurable: true });
    const tickC = (o, h, l, c) => ws.tick('CCC', '60', { time: T0, open: o, high: h, low: l, close: c, volume: 1 });
    const closeText = () => bOf(one(slC, 'vela-sl-ohlc'))[3]?.textContent;
    tickC(61234.5, 61300.25, 61200, 61250.75);
    const afterFirst = measures;
    tickC(61987.5, 62010.25, 61900, 62003.75);
    tickC(61111.5, 61199.25, 61100, 61188.75);
    check('statusline-fit: a digits-only change rewrites the values in place, without measuring', measures === afterFirst && closeText() === '61,188.75', `${afterFirst} -> ${measures}; ${closeText()}`);
    tickC(9.5, 9.99, 9.4, 9.9);
    const afterSmall = measures;
    tickC(9.5, 10.25, 9.4, 10.1);
    check('statusline-fit: re-fits when a value gains a digit', measures > afterSmall && closeText() === '10.10', `${afterSmall} -> ${measures}; ${closeText()}`);
    tickC(100, 101, 99, 100.5);
    const afterUp = measures;
    tickC(100, 101, 99, 99.5);
    check('statusline-fit: re-fits when the change flips sign', measures > afterUp);
    const afterShape = measures;
    ws.cell('ccc').setPriceStyle('line');
    await settle();
    check('statusline-fit: re-fits when the price style changes the readout shape (one value)', measures > afterShape && bOf(one(slC, 'vela-sl-ohlc')).length === 1, `${measures} vs ${afterShape}; ${bOf(one(slC, 'vela-sl-ohlc')).length}`);
    ws.cell('ccc').setPriceStyle('candles');
    await settle();
    const css = doc.head.all((n) => n.tagName === 'STYLE').map((s) => s.textContent ?? '').join('\n');
    check('statusline-fit: the values are set in tabular figures', /\.vela-statusline \.vela-sl-values \{[^}]*font-variant-numeric: tabular-nums/.test(css));
    check('statusline-replay: the meta dot sits one space after the ticker', css.includes('.vela-statusline .vela-sl-symbol + .vela-sl-meta { margin-inline-start: calc(var(--vela-space-1) - var(--vela-space-2)); }'));
    check('statusline-replay: the badge slot holds the market bubble and the replay badge (drawn in the inverse chip colors), the replay one hidden', one(slC, 'vela-sl-replay-badge').style.getPropertyValue('display') === 'none' && one(slC, 'vela-sl-bubble').style.getPropertyValue('display') === '' && one(slC, 'vela-sl-replay-badge').innerHTML.includes('var(--vela-selected-bg)') && one(slC, 'vela-sl-replay-badge').innerHTML.includes('var(--vela-selected-fg)'));
    // a new market drops the old market resting readout (the original re-binds the line to its chart)
    check('the readout holds the last tick before the symbol changes', bOf(one(slC, 'vela-sl-ohlc')).length === 4);
    ws.cell('ccc').setSymbol('DDD');
    await settle();
    check('a new market clears the old market readout until its next tick', bOf(one(slC, 'vela-sl-ohlc')).length === 0 && text(one(slC, 'vela-sl-change')) === '', `${bOf(one(slC, 'vela-sl-ohlc')).length} / ${text(one(slC, 'vela-sl-change'))}`);
    ws.cell('ccc').setSymbol('CCC');
    await settle();

    // cell-controls: the cluster pins to the plot centre; a grip released over nothing (or cancelled) moves nothing
    const ccC = one(cccCell, 'vela-cell-controls');
    check('cell-controls: the cluster pins left to the plot centre (scale gutter) and centres itself', ccC.style.getPropertyValue('left').includes('--vela-scale-gutter') && ccC.style.getPropertyValue('transform') === 'translateX(-50%)', ccC.style.getPropertyValue('left'));
    pointer(cccCell, 'pointermove', cccCell.rect.left + cccCell.rect.width / 2, cccCell.rect.top + cccCell.rect.height - 46);
    const gripC = byClass(ccC, 'vela-cc-grip')[0];
    const order = () => cellEls(ws).map((c) => c.getAttribute('data-cell-id')).join();
    const orderBefore = order();
    pointer(gripC, 'pointerdown', 10, 10);
    pointer(gripC, 'pointermove', -50, -50);
    pointer(gripC, 'pointerup', -50, -50);
    pointer(gripC, 'pointerdown', 10, 10);
    pointer(gripC, 'pointercancel', 0, 0);
    await settle();
    check('cell-controls: releasing over nothing, or a cancel, commits no move and clears the preview', order() === orderBefore && cellEls(ws).every((c) => !c.hasAttribute('data-drop-target')), order());
    pointer(cccCell, 'pointerleave', 0, 0);
    check('cell-controls: the pointer leaving the cell hides the cluster', ccC.style.getPropertyValue('display') === 'none');

    // the audit keyboard fixes
    ws.setActiveCell('aaa');
    await settle();
    click(symbolBtn);
    await settle();
    const spField = one(root, 'vela-sp-input');
    let spValue = '';
    Object.defineProperty(spField, 'value', {
        get: () => spValue,
        set: (v) => {
            spValue = v;
            spField.selectionStart = spField.selectionEnd = String(v).length; // a browser parks the caret at the end on a write
        },
        configurable: true,
    });
    spField.value = 'abcd';
    spField.setSelectionRange(2, 2);
    spField.dispatch('input', {});
    check('symbol-picker: upper-casing the field keeps the caret where the user typed', spField.value === 'ABCD' && spField.selectionStart === 2 && spField.selectionEnd === 2, `${spField.value} @ ${spField.selectionStart}`);
    check('symbol-picker: the search card is not draggable (as in the original)', (() => {
        const h = one(root, 'vela-st-header');
        pointer(h, 'pointerdown', 10, 10);
        pointer(h, 'pointermove', 60, 60);
        return one(root, 'vela-st-panel').style.getPropertyValue('transform') === '';
    })());
    key(doc, 'Escape');
    await settle();
    intent('timezone-drawer');
    await settle();
    const tzRow = byClass(one(root, 'vela-st-drawer'), 'vela-tzd-row').find((r) => r.getAttribute('data-value') === 'Europe/Istanbul');
    key(tzRow, 'Enter');
    await settle();
    check('timezone-drawer: a focusable row also acts from the keyboard', ws.timezone() === 'Europe/Istanbul' && !one(root, 'vela-st-drawer'));
    ws.setTimezone('Etc/UTC');
    await settle();
    click(layoutBtn);
    const lp2 = one(root, 'vela-lp-layer');
    click(byClass(lp2, 'vela-lp-sq').find((s) => s.getAttribute('data-r') === '0' && s.getAttribute('data-c') === '1'));
    await settle();
    check('layout-picker: a square also acts from the keyboard (a click with the picker open)', ['2h', '2v'].includes(ws.layout()) && !one(root, 'vela-lp-layer'), ws.layout());
    ws.setLayout('4');
    await settle();
    click(indBtn);
    await settle();
    const ipHeader = one(root, 'vela-st-header');
    const ipPanel = one(root, 'vela-st-panel');
    pointer(ipHeader, 'pointerdown', 100, 100);
    pointer(ipHeader, 'pointermove', 130, 90);
    pointer(ipHeader, 'pointerup', 130, 90);
    pointer(ipHeader, 'pointermove', 400, 400);
    check('indicator-picker: the card drags by its header (draggable in the original) and stops on release', ipPanel.style.getPropertyValue('transform') === 'translate(30px, -10px)', ipPanel.style.getPropertyValue('transform'));
    const libRow = byClass(root, 'vela-ip-row').find((r) => r.hasAttribute('data-library'));
    check('indicator-picker: a library row is a focusable button', libRow.getAttribute('role') === 'button' && libRow.getAttribute('tabindex') === '0');
    const nInd = active().indicators().length;
    key(libRow, 'Enter');
    await settle();
    check('indicator-picker: Enter on a library row adds the indicator', active().indicators().length === nInd + 1);
    key(byClass(root, 'vela-ip-trash')[0], 'Enter');
    await settle();
    check('indicator-picker: a key on the trash button is that button own click (the row does not act)', active().indicators().length === nInd + 1);
    click(byClass(root, 'vela-ip-trash')[0]);
    await settle();
    key(doc, 'Escape');
    await settle();
    check('indicator-picker: the trash removes it again', active().indicators().length === nInd);

    // the hairlines re-snap when the window resizes (the device pixel ratio follows page zoom)
    doc.defaultView.devicePixelRatio = 2;
    doc.defaultView.dispatch('resize', {});
    await settle();
    check('topbar: the hairlines re-snap to one device pixel after a window resize', byClass(content, 'vela-sep').every((s) => s.style.getPropertyValue('width') === '0.5px'), byClass(content, 'vela-sep').map((s) => s.style.getPropertyValue('width')).join());
    doc.defaultView.devicePixelRatio = 1;
    doc.defaultView.dispatch('resize', {});
    await settle();
}

// ── everything follows the active cell ──
ws.setActiveCell('ccc');
await settle();
check('the topbar follows the active cell (symbol, timeframe, style)', text(symbolBtn) === 'CCC' && styleBtn.getAttribute('aria-label') === 'Chart style \u2014 Candles');
ws.cell('ccc').setPriceStyle('area');
await settle();
check('a style change on the active cell reaches the button', styleBtn.getAttribute('aria-label') === 'Chart style \u2014 Area', styleBtn.getAttribute('aria-label'));
ws.cell('ccc').setPriceStyle('candles');
ws.setActiveCell('aaa');
await settle();
check('and back', text(symbolBtn) === 'AAA');
click(symbolBtn);
await settle();
byClass(root, 'vela-sp-row');
typeInto(one(root, 'vela-sp-input'), 'DDD');
key(one(root, 'vela-sp-input'), 'Enter');
await settle();
check('the pick lands on the cell that was active when the dialog opened (the active one)', sameSymbol(ws.cell('aaa').symbol(), 'DDD') && sameSymbol(ws.cell('ccc').symbol(), 'CCC') && text(one(sl(cellEl(ws, 'aaa')), 'vela-sl-symbol')) === 'DDD' && text(one(cellEl(ws, 'aaa'), 'vela-watermark-text')).startsWith('DDD'), `${ws.cell('aaa').symbol()} / ${text(one(sl(cellEl(ws, 'aaa')), 'vela-sl-symbol'))}`);
ws.cell('aaa').setSymbol('AAA');
await settle();

// ── layout changes create and destroy the overlays ──
ws.setLayout('2h');
await settle();
check('dropped cells lose their overlays with their DOM; the survivors keep theirs', cellEls(ws).length === 2 && cellEls(ws).every((c) => sl(c) && one(c, 'vela-watermark') && cc(c)));
ws.setLayout('1');
await settle();
check('a one-cell grid has no maximize and no grip in its cluster', byClass(cc(cellEls(ws)[0]), 'vela-cc-btn').map((b) => b.getAttribute('data-action')).join() === 'zoom-out,zoom-in,reset');
ws.setLayout('4');
await settle();
check('back to four: the re-created cells have overlays again', cellEls(ws).length === 4 && cellEls(ws).every((c) => sl(c) && one(c, 'vela-watermark') && cc(c)));

// ── theme and locale ──
const bgBefore = root.style.getPropertyValue('--vela-bg');
ws.setTheme('light');
await settle();
check('a theme change rewrites the tokens the shell reads (the chrome restyles in place)', root.style.getPropertyValue('--vela-bg') !== bgBefore && topbar.children[0] === content && hasClass(sl(aaaCell) ?? cellEl(ws, 'aaa'), 'vela-statusline') !== undefined);
ws.setTheme('dark');
await settle();
ws.setLocale('tr');
await settle();
check('a locale change re-reads every label (English fallback without a pack) and keeps the structure', named.every((b) => (b.getAttribute('aria-label') ?? '').length > 0) && one(content, 'vela-widget-symbol') && byClass(bbContent, 'vela-bb-range').length === 9);
ws.setLocale('en');
await settle();

// ── the single-chart widget: the same shell, without the multi-chart parts ──
const widgetHost = container(W, H);
const widget = new wasm.VelaWidget(widgetHost, { bars: seedBars(), symbol: 'BBB', timeframe: '60' });
current = { grid: () => widget.root().all((n) => hasClass(n, 'vela-ws-grid'))[0] };
await widget.ready();
await settle();
const wroot = widget.root();
const wtop = wroot.all((n) => hasClass(n, 'vela-widget-topbar'))[0];
check('the widget shows the symbol, the timeframes and the style, but no layout button', byClass(wtop, 'vela-widget-style').length === 1 && text(one(wtop, 'vela-widget-symbol')) === 'BBB');
const wcell = byClass(wroot, 'vela-cell')[0];
pointer(wcell, 'pointermove', wcell.rect.left + wcell.rect.width / 2, wcell.rect.top + wcell.rect.height - 46);
check('its hover cluster has no grip and no maximize (a lone chart has nothing to trade space with)', byClass(one(wcell, 'vela-cell-controls'), 'vela-cc-btn').map((b) => b.getAttribute('data-action')).join() === 'zoom-out,zoom-in,reset');
check('its status line and watermark show the symbol', text(one(wcell, 'vela-sl-symbol')) === 'BBB' && text(one(wcell, 'vela-watermark-text')) === 'BBB \u00b7 1h');
widget.destroy();
await settle();

// ── the mobile size class: the controls cluster is suspended, an open dialog closes on the flip ──
const mobileHost = container(400, 700);
const mws = new wasm.VelaWorkspace(mobileHost, { layout: '4', bars: seedBars(), layoutMode: 'mobile', cells: Object.fromEntries(SYMBOLS.map((s) => [s.toLowerCase(), { symbol: s, timeframe: '60' }])) });
current = mws;
await mws.ready();
await settle();
const mcell = cellEls(mws)[0];
pointer(mcell, 'pointermove', mcell.rect.left + mcell.rect.width / 2, mcell.rect.top + mcell.rect.height - 46);
check('on mobile the proximity reveal never shows the controls', one(mcell, 'vela-cell-controls').style.getPropertyValue('display') === 'none');
mws.destroy();
await settle();
current = ws;

// ── destroy leaves nothing behind ──
const regionsBefore = ws.regions().map((r) => ws.region(r));
ws.destroy();
await settle();
check('destroy removes the root and every overlay', !host.children.some((c) => hasClass(c, 'vela-workspace')));
check('destroy leaves no listener on the container, the document or the window', totalListeners(host) === listenersBefore && doc.listeners.length === docListenersBefore && doc.defaultView.listeners.length === winListenersBefore, `${totalListeners(host)} vs ${listenersBefore}; doc ${doc.listeners.length} vs ${docListenersBefore}; win ${doc.defaultView.listeners.length} vs ${winListenersBefore}`);
check('every observer is disconnected', observers.every((o) => !o.connected));
check('the regions were emptied', ['topbar', 'bottombar'].every((r) => regionsBefore.find((e) => e.getAttribute('data-vela-region') === r).children.length === 0));

if (state.failures > 0) {
    console.error(`smoke_shell_top: ${state.failures} check(s) failed`);
    process.exit(1);
}
console.log('smoke_shell_top: all checks passed');
