// Smoke test of vela-web's in-chart chrome (the legend, the pane controls, the A / L axis
// buttons, the scroll-to-latest button, the tooltips and the menus) in Node, against the
// fake DOM of fake_dom.mjs, on both the direct chart (VelaChart) and the facade (VelaApp).
//
// It does NOT run a browser: real layout, hover, focus, painting and the CSS are not
// exercised. The timeline-mark popup is not reachable from the JavaScript API (marks are
// not exposed), so it is not covered here.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_legend.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, check, clock, container, doc, frames, loadPackage, makeBars, painted, pileOf, pump, state, tick } from './fake_dom.mjs';

const wasm = await loadPackage('smoke_legend.mjs');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** Frames, then the timers the chrome defers to (graveyard purge, tips), then frames again. */
async function settle(ms = 15) {
    pump();
    await sleep(ms);
    pump();
}

const byClass = (root, cls) => root.all((n) => n.className === cls || (typeof n.className === 'string' && n.className.split(' ').includes(cls)));
const rowsOf = (root) => byClass(root, 'vela-ind-row');
const titleOf = (row) => byClass(row, 'vela-ind-title')[0]?.textContent ?? '';
const valuesOf = (row) => byClass(row, 'vela-ind-values')[0];
const ctlOf = (row, name) => row.all((n) => n.getAttribute?.('data-vela-ctl') === name)[0];
const rowFor = (root, re) => rowsOf(root).find((r) => re.test(titleOf(r)));
const legends = (root) => byClass(root, 'vela-legend');
const legendOf = (root, pane) => legends(root).find((l) => l.getAttribute('data-vela-pane') === pane);
const menusOf = (root) => byClass(root, 'vela-cmenu');
const click = (node) => node.dispatch('click', { button: 0, clientX: 10, clientY: 10 });
const move = (data, x, y, type = 'mouse') => data.dispatch('pointermove', { clientX: x, clientY: y, pointerId: 1, pointerType: type, button: 0, timeStamp: clock });
const strokes = (canvas) => canvas.ctx.ops.filter((op) => op[0] === 'stroke').length;

async function scenario(label, host, data, { engineWired }) {
    const ck = (name, ok, detail) => check(`${label}: ${name}`, ok, detail);
    await settle();

    // ── the legend ──
    const rows = rowsOf(host);
    ck('a legend row per mounted indicator', rows.length >= 3, rows.map(titleOf).join(' | '));
    const ema = rowFor(host, /EMA/i);
    const rsi = rowFor(host, /RSI/i);
    const macd = rowFor(host, /MACD/i);
    ck('rows carry their indicator titles and an accessible name', ema && rsi && macd && ema.getAttribute('aria-label') === titleOf(ema) && ema.getAttribute('role') === 'group' && ema.getAttribute('tabindex') === '0');
    const priceLegend = legendOf(host, 'price');
    ck('overlay rows sit in the price pane column, studies in their own', priceLegend?.children.includes(ema) && !priceLegend.children.includes(rsi) && legends(host).length === 3, legends(host).map((l) => l.getAttribute('data-vela-pane')).join(','));
    const rsiLegend = rsi.parentNode;
    const tops = legends(host).map((l) => Number.parseFloat(l.style.getPropertyValue('top')));
    ck('each column is pinned at its pane top (positions are numbers, studies lower)', tops.every(Number.isFinite) && Number.parseFloat(rsiLegend.style.getPropertyValue('top')) > Number.parseFloat(priceLegend.style.getPropertyValue('top')), tops.join(','));
    ck('the legend is a labelled group', legends(host).every((l) => l.getAttribute('role') === 'group' && l.getAttribute('aria-label')));

    // Live values.
    const v0 = valuesOf(ema);
    ck('live plot values show beside the title, colored', v0.style.getPropertyValue('display') === 'block' && v0.textContent !== '' && v0.all((n) => n.style.getPropertyValue('color') !== '').length > 0, v0.textContent);
    move(data, 300, 200);
    await settle();
    const textA = valuesOf(ema).textContent;
    move(data, 640, 200);
    await settle();
    const textB = valuesOf(ema).textContent;
    ck('the values follow the crosshair', textA !== '' && textB !== '' && textA !== textB, `${textA} -> ${textB}`);
    data.dispatch('pointerleave', {});
    await settle();

    // Opening a row: title hover reveals the controls and steps the values aside.
    const wrap = byClass(ema, 'vela-ind-title-wrap')[0];
    wrap.dispatch('mouseenter', { bubbles: false });
    ck('hovering the title opens the row (controls out, values aside)', ema.getAttribute('data-open') === '1' && valuesOf(ema).style.getPropertyValue('display') === 'none');
    ck('every control has an accessible name', ['eye', 'settings', 'move', 'remove'].every((c) => ctlOf(ema, c)?.getAttribute('aria-label')), ['eye', 'settings', 'move', 'remove'].map((c) => ctlOf(ema, c)?.getAttribute('aria-label')).join('|'));
    ck('the settings gear exists for an indicator with inputs', ctlOf(ema, 'settings').style.getPropertyValue('display') !== 'none');
    let threw = false;
    try {
        click(ctlOf(ema, 'settings'));
    } catch {
        threw = true;
    }
    ck('the gear calls the renderer (no dialog host: a no-op, no throw)', !threw);
    ema.dispatch('mouseleave', { bubbles: false });
    ck('leaving the row closes it', ema.getAttribute('data-open') === '0');

    // Selection and keyboard.
    ema.dispatch('keydown', { key: 'Enter', target: ema });
    ck('Enter on a focused row selects it (controls reachable by keyboard)', ema.getAttribute('data-open') === '1');
    ema.dispatch('keydown', { key: 'Escape', target: ema });
    ck('Escape clears the selection', ema.getAttribute('data-open') === '0');

    // Tooltip on a control: a mouse hover opens it after the delay.
    const gear = ctlOf(ema, 'settings');
    gear.dispatch('pointerenter', { pointerType: 'mouse', bubbles: false });
    await sleep(760);
    const tips = byClass(host, 'vela-chrome-tip');
    ck('a control tip opens after the hover delay', tips.length === 1 && tips[0].textContent === 'Settings' && tips[0].getAttribute('role') === 'tooltip', tips.map((t) => t.textContent).join('|'));
    gear.dispatch('pointerleave', { bubbles: false });
    ck('and closes on leave', byClass(host, 'vela-chrome-tip').length === 0);
    gear.dispatch('pointerenter', { pointerType: 'touch', bubbles: false });
    await sleep(760);
    ck('a touch never opens a tip', byClass(host, 'vela-chrome-tip').length === 0);
    gear.dispatch('pointerleave', { bubbles: false });

    // The row context menu: values toggle.
    ema.dispatch('contextmenu', { clientX: 100, clientY: 50 });
    let menus = menusOf(host);
    ck('right-click opens the row menu with the values checkbox', menus.length === 1 && menus[0].getAttribute('role') === 'menu' && menus[0].all((n) => n.getAttribute?.('role') === 'menuitemcheckbox').length === 1);
    const stackItems = menus[0].all((n) => (n.getAttribute?.('data-vela-item') ?? '').startsWith('stack:'));
    ck('the overlay row offers restacking', stackItems.length >= 1, stackItems.map((n) => n.getAttribute('data-vela-item')).join(','));
    const valuesItem = menus[0].all((n) => n.getAttribute?.('data-vela-item') === 'values')[0];
    menus[0].dispatch('keydown', { key: 'ArrowDown' });
    click(valuesItem);
    await settle();
    ck('picking "Indicator values" hides this row\'s values and closes the menu', menusOf(host).length === 0 && valuesOf(ema).style.getPropertyValue('display') === 'none');
    ema.dispatch('contextmenu', { clientX: 100, clientY: 50 });
    const again = menusOf(host)[0];
    again.dispatch('keydown', { key: 'Escape' });
    await settle();
    ck('Escape closes a menu', menusOf(host).length === 0);
    // restore this row's values
    ema.dispatch('contextmenu', { clientX: 100, clientY: 50 });
    click(menusOf(host)[0].all((n) => n.getAttribute?.('data-vela-item') === 'values')[0]);
    await settle();
    move(data, 301, 200);
    await settle();
    ck('toggling again restores them', valuesOf(ema).style.getPropertyValue('display') === 'block');
    data.dispatch('pointerleave', {});

    // ── pane controls ──
    move(data, 400, 520 - 120); // inside a study pane
    await settle();
    const clusters = byClass(host, 'vela-pc');
    ck('a cluster per pane', clusters.length === 3, `${clusters.length}`);
    const shown = clusters.filter((c) => c.style.getPropertyValue('display') === 'flex' && c.getAttribute('data-rest') === '0');
    ck('only the hovered pane\'s cluster is revealed', shown.length === 1);
    const cluster = shown[0];
    const roleBtn = (c, role) => c.all((n) => n.getAttribute?.('data-role') === role)[0];
    ck('study cluster: up, down, collapse, maximize, each named', ['up', 'down', 'collapse', 'maximize'].every((r) => roleBtn(cluster, r)?.getAttribute('aria-label')), cluster.children.map((b) => b.getAttribute('aria-label')).join('|'));
    const nameBefore = roleBtn(cluster, 'collapse').getAttribute('aria-label');
    click(roleBtn(cluster, 'collapse'));
    await settle();
    move(data, 400, 520 - 120);
    await settle();
    const collapsedCluster = byClass(host, 'vela-pc').find((c) => roleBtn(c, 'collapse')?.getAttribute('aria-label') === 'Expand panel');
    ck('collapse collapses the pane (the button turns into Expand)', nameBefore === 'Collapse panel' && collapsedCluster);
    click(roleBtn(collapsedCluster, 'collapse'));
    await settle();
    ck('expand restores it', byClass(host, 'vela-pc').every((c) => roleBtn(c, 'collapse')?.getAttribute('aria-label') !== 'Expand panel'));
    // maximize
    move(data, 400, 520 - 120);
    await settle();
    const shown2 = byClass(host, 'vela-pc').filter((c) => c.getAttribute('data-rest') === '0' && c.style.getPropertyValue('display') === 'flex')[0];
    click(roleBtn(shown2, 'maximize'));
    await settle();
    ck('maximize isolates a pane (its restore chip stays visible without hover)', byClass(host, 'vela-pc').some((c) => roleBtn(c, 'maximize')?.getAttribute('aria-label') === 'Restore panel' && c.getAttribute('data-rest') === '0'));
    const restore = byClass(host, 'vela-pc').find((c) => roleBtn(c, 'maximize')?.getAttribute('aria-label') === 'Restore panel');
    click(roleBtn(restore, 'maximize'));
    await settle();
    ck('restore brings the panes back', byClass(host, 'vela-pc').every((c) => roleBtn(c, 'maximize')?.getAttribute('aria-label') === 'Maximize panel'));
    // move down: RSI is the first study, so up is disabled and down is not.
    const rsiPane = rsi.parentNode.getAttribute('data-vela-pane');
    const clusterOfRsi = () => {
        const hostOfPane = byClass(host, 'vela-pc');
        return hostOfPane.find((c) => roleBtn(c, 'up') && roleBtn(c, 'up').getAttribute('disabled') !== null && roleBtn(c, 'down')?.getAttribute('disabled') === null);
    };
    const first = clusterOfRsi();
    ck('the first study cannot move up, can move down', Boolean(first));
    click(roleBtn(first, 'down'));
    await settle();
    ck('move down reorders the panes (the order of the study columns swaps)', (() => {
        const order = legends(host).map((l) => l.getAttribute('data-vela-pane')).filter((p) => p !== 'price');
        const topsNow = Object.fromEntries(legends(host).map((l) => [l.getAttribute('data-vela-pane'), Number.parseFloat(l.style.getPropertyValue('top'))]));
        return order.length === 2 && topsNow[rsiPane] > Math.min(...Object.values(topsNow).filter((_, i) => i > 0));
    })());
    data.dispatch('pointerleave', {});
    await settle();
    ck('leaving the plot hides the clusters', byClass(host, 'vela-pc').every((c) => c.getAttribute('data-rest') === '1' || c.style.getPropertyValue('display') === 'none'));

    // ── A / L axis buttons ──
    const axis = byClass(host, 'vela-axis')[0];
    ck('the A / L strip exists with named buttons', axis && axis.children.length === 2 && axis.children.every((b) => b.getAttribute('aria-label')));
    ck('it rests invisibly on the price scale (keyboard reachable)', axis.getAttribute('data-rest') === '1' && axis.style.getPropertyValue('display') === 'flex');
    move(data, 900 - 20, 200); // over the master scale column of the price pane
    await settle();
    ck('hovering the price scale reveals it', axis.getAttribute('data-rest') === '0' && axis.style.getPropertyValue('display') === 'flex');
    const [autoBtn, logBtn] = axis.children;
    ck('auto starts on, log off', autoBtn.getAttribute('aria-pressed') === 'true' && logBtn.getAttribute('aria-pressed') === 'false');
    click(logBtn);
    await settle();
    ck('L toggles the log scale', logBtn.getAttribute('aria-pressed') === 'true');
    click(logBtn);
    click(autoBtn);
    await settle();
    ck('A toggles autoscale off (the pane freezes its window)', autoBtn.getAttribute('aria-pressed') === 'false');
    move(data, 900 - 20, 200);
    await sleep(0);
    click(autoBtn);
    await settle();
    ck('and on again', autoBtn.getAttribute('aria-pressed') === 'true');
    data.dispatch('pointerleave', {});
    await settle();

    // ── scroll to latest ──
    const scrollBtn = byClass(host, 'vela-scroll-latest')[0];
    ck('the scroll button exists, named, hidden at the latest bar', scrollBtn && scrollBtn.getAttribute('aria-label') === 'Scroll to most recent bar' && scrollBtn.style.getPropertyValue('display') === 'none');
    const offset0 = chart_viewport(host)[1];
    data.dispatch('pointerdown', { clientX: 300, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
    for (let i = 1; i <= 24; i += 1) {
        tick(8);
        data.dispatch('pointermove', { clientX: 300 + i * 25, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
    }
    data.dispatch('pointerup', { clientX: 900, clientY: 200, pointerId: 7, pointerType: 'mouse', button: 0, timeStamp: clock });
    await settle();
    const offsetAway = chart_viewport(host)[1];
    ck('panning away leaves the latest bar behind', offsetAway < offset0, `${offset0.toFixed(1)} -> ${offsetAway.toFixed(1)}`);
    ck('the button stays hidden while the pointer is far', scrollBtn.style.getPropertyValue('display') === 'none');
    const right = Number.parseFloat(scrollBtn.style.getPropertyValue('right'));
    const bottom = Number.parseFloat(scrollBtn.style.getPropertyValue('bottom'));
    move(data, 900 - right - 13, 560 - bottom - 13);
    await settle();
    ck('it appears with the pointer near it, at the published spot', scrollBtn.style.getPropertyValue('display') === 'flex' && Number.isFinite(Number.parseFloat(scrollBtn.style.getPropertyValue('bottom'))), `${scrollBtn.style.getPropertyValue('right')} ${scrollBtn.style.getPropertyValue('bottom')}`);
    click(scrollBtn);
    for (let i = 0; i < 20; i += 1) {
        await settle(2);
        tick(50);
    }
    const offsetBack = chart_viewport(host)[1];
    ck('pressing it glides the view back to the latest bar', offsetBack >= 0 && offsetBack > offsetAway, `${offsetAway.toFixed(1)} -> ${offsetBack.toFixed(1)}`);
    data.dispatch('pointerleave', {});
    await settle();
    ck('and it goes away again', scrollBtn.style.getPropertyValue('display') === 'none');

    // A label tooltip lookup with nothing under the pointer opens nothing.
    move(data, 400, 100);
    await sleep(400);
    ck('no label under the pointer: no tooltip', byClass(host, 'vela-chrome-tip').length === 0);
    data.dispatch('pointerleave', {});

    // ── the move menu ──
    const rsiRow = rowFor(host, /RSI/i);
    rsiRow.dispatch('mouseenter', { bubbles: false });
    byClass(rsiRow, 'vela-ind-title-wrap')[0].dispatch('mouseenter', { bubbles: false });
    click(ctlOf(rsiRow, 'move'));
    menus = menusOf(host);
    const labels = menus[0]?.all((n) => n.getAttribute?.('role') === 'menuitem').map((n) => n.children.at(-1)?.textContent) ?? [];
    ck('the move menu lists the pane moves', menus.length === 1 && labels.includes('Move to main chart') && labels.some((l) => /^Move to /.test(l) && l !== 'Move to main chart'), labels.join(' | '));
    const toMain = menus[0].all((n) => n.getAttribute?.('role') === 'menuitem' && n.children.at(-1)?.textContent === 'Move to main chart')[0];
    click(toMain);
    await settle();
    if (engineWired) {
        ck('moving to the main chart puts the row in the price column and drops the empty pane column', legendOf(host, 'price')?.children.includes(rowFor(host, /RSI/i)) && legends(host).length === 2, legends(host).map((l) => l.getAttribute('data-vela-pane')).join(','));
    } else {
        console.log('skip  VelaChart move: the engine is not between the renderer and the chart (the dialog agent wires the notify_* subscriptions)');
    }

    // ── the eye ──
    const emaRow = rowFor(host, /EMA/i);
    byClass(emaRow, 'vela-ind-title-wrap')[0].dispatch('mouseenter', { bubbles: false });
    move(data, 500, 200);
    await settle();
    data.ctx.ops = [];
    const before = strokes(data);
    click(ctlOf(emaRow, 'eye'));
    await settle();
    if (engineWired) {
        const hiddenRow = rowFor(host, /EMA/i);
        ck('the eye hides the indicator: row dimmed, label flips to Show, readout cleared', hiddenRow.getAttribute('data-hidden') === '1' && ctlOf(hiddenRow, 'eye').getAttribute('aria-label') === 'Show' && valuesOf(hiddenRow).textContent === '');
        click(ctlOf(hiddenRow, 'eye'));
        await settle();
        const shownRow = rowFor(host, /EMA/i);
        ck('and shows it again', shownRow.getAttribute('data-hidden') === '0' && ctlOf(shownRow, 'eye').getAttribute('aria-label') === 'Hide');
    } else {
        console.log(`skip  VelaChart eye: no engine behind the renderer (${before} strokes)`);
    }

    // ── remove ──
    const count = rowsOf(host).length;
    const target = rowFor(host, /EMA/i);
    byClass(target, 'vela-ind-title-wrap')[0].dispatch('mouseenter', { bubbles: false });
    click(ctlOf(target, 'remove'));
    await settle();
    if (engineWired) {
        ck('remove removes the row (and the engine\'s indicator)', rowsOf(host).length === count - 1 && !rowFor(host, /EMA/i));
    } else {
        console.log(`skip  VelaChart remove: rows ${count} -> ${rowsOf(host).length} (the subscription belongs to the dialog agent)`);
    }
    const fold = byClass(host, 'vela-ind-fold')[0];
    ck('the fold chip sits under the price column with 2+ indicators, named', !fold || (fold.getAttribute('aria-label') && fold.getAttribute('aria-expanded') !== null));
    if (fold) {
        click(fold);
        await settle();
        ck('the chip folds every column behind its count', rowsOf(host).every((r) => r.style.getPropertyValue('display') === 'none') && byClass(host, 'vela-ind-fold-count').length === 1);
        click(byClass(host, 'vela-ind-fold')[0]);
        await settle();
        ck('and unfolds again', rowsOf(host).every((r) => r.style.getPropertyValue('display') !== 'none'));
    }
}

function chart_viewport(host) {
    return host.__chart.viewport();
}

function mount(Ctor, ...args) {
    const el = container(900, 560);
    const chart = new Ctor(el, ...args);
    el.__chart = chart;
    return { el, chart };
}

// ── VelaChart ──

{
    const { el, chart } = mount(wasm.VelaChart, 'dark');
    chart.setBars(makeBars(400));
    chart.addVolume();
    chart.addIndicator('ema');
    chart.addIndicator('rsi');
    chart.addIndicator('macd');
    const { canvases } = pileOf(el);
    const data = canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
    await scenario('VelaChart', el, data, { engineWired: true });
    chart.destroy();
    pump();
    await sleep(15);
    check('VelaChart: destroy removes the chrome root, its listeners and frames', byClass(el, 'vela-chrome-ui').length === 0 && doc.listeners.length === 0 && frames.size === 0, `document listeners ${doc.listeners.length}`);
}

// ── stacking order and callout panel structure (InputsUI.orderRows, CalloutBubble) ──

{
    const { el, chart } = mount(wasm.VelaChart, 'dark');
    chart.setBars(makeBars(300));
    chart.addIndicator('ema');
    chart.addIndicator('sma');
    await settle();
    const price = legendOf(el, 'price');
    const titles = rowsOf(price).map(titleOf);
    check('legend-stacking: the price column lists the front-most overlay first (a new indicator mounts behind the others)', titles.length === 2 && /EMA/i.test(titles[0]) && /SMA/i.test(titles[1]), titles.join(' | '));
    const children = price.children.map((c) => (c.className.includes('vela-ind-fold') ? '[fold]' : 'row'));
    check('legend-stacking: the fold chip stays last in the column', children.join(',') === 'row,row,[fold]', children.join(','));
    chart.destroy();
    pump();
    await sleep(15);
}

// ── VelaApp ──

{
    const el = container(900, 560);
    const app = new wasm.VelaApp(el, 'dark', 'DEMO', '1h', makeBars(300));
    el.__chart = app;
    await app.ready();
    pump();
    app.addIndicator('ema', undefined);
    app.addIndicator('rsi', undefined);
    app.addIndicator('macd', undefined);
    const { canvases } = pileOf(el);
    const data = canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
    await scenario('VelaApp', el, data, { engineWired: true });
    app.destroy();
    pump();
    await sleep(15);
    check('VelaApp: destroy removes the chrome root, its listeners and frames', byClass(el, 'vela-ind-row').length === 0 && byClass(el, 'vela-chrome-ui').length === 0 && doc.listeners.length === 0 && frames.size === 0, `document listeners ${doc.listeners.length}`);
}

if (state.failures > 0) {
    console.log(`${state.failures} check(s) failed.`);
    process.exit(1);
}
console.log('vela-web chrome (legend, pane controls, axis buttons, scroll button, tips, menus) behaves as expected against the fake DOM (no browser was run).');
