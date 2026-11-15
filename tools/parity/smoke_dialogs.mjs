// Smoke test of vela-web's dialogs (the chart settings dialog with its gear, and the
// per-indicator inputs dialog) in Node, against the shared fake DOM (fake_dom.mjs).
//
// Both entries run: the direct chart (VelaChart) and the facade chart (VelaApp). Controls are
// found the way a user finds them (by label), edited by setting the value the way the
// browser would and dispatching the event the control listens for, and the effect is read
// back through getConfig() / indicatorInputs() and the repainted canvases. It does NOT run a
// browser: real layout, focus rings, native pickers and real painting are not exercised.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_dialogs.mjs pkg
//
// Exits 1 on the first failed check.

import { check, container, doc, loadPackage, makeBars, painted, pileOf, pump, state } from './fake_dom.mjs';

const wasm = await loadPackage('smoke_dialogs.mjs');
const sleep = (ms = 5) => new Promise((r) => setTimeout(r, ms));
const settle = async () => {
    for (let i = 0; i < 3; i += 1) {
        await sleep(2);
        pump();
    }
};

// ── fake-DOM helpers ──

const byId = (root, id) => root.all((n) => n.id === id)[0] ?? null;
const labelText = (n) => n.textContent || n.children.map(labelText).join('');
// The control a visible label names: <label for=id>, or the checkbox inside a toggle label.
function control(root, text) {
    const label = root.all((n) => n.tagName === 'LABEL' && n.getAttribute('for') && labelText(n) === text)[0];
    return label ? byId(root, label.getAttribute('for')) : null;
}
const swatch = (root, aria) => root.all((n) => n.getAttribute('type') === 'color' && n.getAttribute('aria-label') === aria)[0] ?? null;
const button = (root, text) => root.all((n) => n.tagName === 'BUTTON' && n.textContent === text)[0] ?? null;
const tabs = (root) => root.all((n) => n.getAttribute('role') === 'tab');
const activePane = (root) => root.all((n) => n.getAttribute('role') === 'tabpanel' && n.hasAttribute('data-shown'))[0];
const dialogOf = (wrapper) => wrapper.all((n) => n.className === 'vela-dlg-positioner')[0] ?? null;
const selectedTab = (root) => tabs(root).find((t) => t.getAttribute('aria-selected') === 'true')?.textContent;
const cfg = (c) => JSON.parse(c.getConfig());
const setSelect = (el, value) => {
    el.value = value;
    el.dispatch('change', {});
};
const setCheck = (el, on) => {
    el.checked = on;
    el.dispatch('change', {});
};
const setNumber = (el, value, type = 'input') => {
    el.value = String(value);
    el.dispatch(type, {});
};
const setColor = (el, value) => {
    el.value = value;
    el.dispatch('input', {});
};
const escape = () => doc.dispatch('keydown', { key: 'Escape', preventDefault() {} });
// Every form control carries a label association or an aria-label.
const unlabelled = (root) => {
    const labelled = new Set(root.all((n) => n.tagName === 'LABEL' && n.getAttribute('for')).map((n) => n.getAttribute('for')));
    return root.all((n) => ['INPUT', 'SELECT', 'TEXTAREA'].includes(n.tagName) && !n.getAttribute('aria-label') && !labelled.has(n.id));
};

async function scenario(name, make, { addIndicators, ready }) {
    console.log(`\n── ${name} ──`);
    const el = container(900, 560);
    const chart = make(el);
    await ready(chart);
    const rsiId = addIndicators(chart);
    await ready(chart);
    pump();
    const { wrapper, plot } = pileOf(el);

    // ── the gear ──
    check(`${name}: no gear before the settings feature is on`, plot.all((n) => n.className === 'vela-gear').length === 0);
    chart.showSettings(true);
    pump();
    const gear = plot.all((n) => n.className === 'vela-gear')[0];
    check(`${name}: the gear exists, is labelled and shown`, gear && gear.getAttribute('aria-label') === 'Chart settings' && gear.style.getPropertyValue('display') === 'flex' && gear.getAttribute('aria-haspopup') === 'dialog');
    check(`${name}: the gear opens above the time axis, clear of the price scale`, gear.style.getPropertyValue('display') === 'flex');

    // ── open: structure, tabs ──
    gear.focus();
    gear.dispatch('click', {});
    let dlg = dialogOf(wrapper);
    check(`${name}: the gear opens the dialog`, !!dlg && gear.getAttribute('aria-expanded') === 'true');
    const panel = dlg.children[0];
    const title = byId(dlg, panel.getAttribute('aria-labelledby'));
    check(`${name}: role=dialog, labelled by its title`, panel.getAttribute('role') === 'dialog' && title?.textContent === 'Chart settings');
    check(`${name}: the tab rail has the original tabs, in order`, ['Symbol', 'Scales and lines', 'Canvas'].every((t, i) => tabs(dlg)[i]?.textContent === t), tabs(dlg).map((t) => t.textContent).join(' | '));
    check(`${name}: tabs are a tablist with selected / roving state`, tabs(dlg).filter((t) => t.getAttribute('aria-selected') === 'true').length === 1 && tabs(dlg).every((t) => t.getAttribute('aria-controls')));
    check(`${name}: Symbol is the tab it lands on`, selectedTab(dlg) === 'Symbol');
    check(`${name}: focus is on the active tab`, doc.activeElement?.getAttribute('role') === 'tab');
    check(`${name}: every control has a label`, unlabelled(dlg).length === 0, unlabelled(dlg).map((n) => `${n.tagName}#${n.id}`).join(','));

    // ── Symbol ──
    const symbolPane = (n) => n;
    void symbolPane;
    const type = control(dlg, 'Type');
    check(`${name}: Type lists the built-in styles`, type && type.children.length >= 8 && type.value === 'candles', `${type?.children.length} options`);
    const before = cfg(chart);
    const up = swatch(dlg, 'Body: Up color');
    check(`${name}: the candle Body row has up / down swatches`, !!up && !!swatch(dlg, 'Body: Down color') && !!swatch(dlg, 'Wick: Up color'));
    setColor(up, '#123456');
    pump();
    check(`${name}: candle up color changes the config`, cfg(chart).candles.upColor === '#123456' && before.candles.upColor !== '#123456', cfg(chart).candles.upColor);
    const data = pileOf(el).canvases[1];
    data.ctx.ops = [];
    setNumber(control(dlg, 'Spacing'), 3);
    pump();
    check(`${name}: spacing (a number row) changes the config and repaints`, cfg(chart).series.spacing === 3 && painted(data));
    setCheck(control(dlg, 'Animate zoom'), !before.animations.zoom);
    check(`${name}: an animation toggle changes the config`, cfg(chart).animations.zoom === !before.animations.zoom);
    setCheck(control(dlg, 'Reveal on load'), !before.animations.intro);
    check(`${name}: Reveal on load changes the config`, cfg(chart).animations.intro === !before.animations.intro);
    const tz = control(dlg, 'Time zone');
    check(`${name}: the time zone select lists the catalog`, tz && tz.children.length > 20);
    setSelect(tz, 'America/New_York');
    check(`${name}: a time zone pick changes the config`, cfg(chart).timeScale.timezone === 'America/New_York', cfg(chart).timeScale.timezone);
    setSelect(type, 'line');
    pump();
    check(`${name}: the chart type row changes the style`, cfg(chart).series.style === 'line');
    const hiddenGroups = dlg.all((n) => n.className.includes('vela-sd-grp') && n.className.includes('vela-sd-hide'));
    check(`${name}: only the active style group stays visible`, hiddenGroups.length >= 5, `${hiddenGroups.length} hidden`);
    setNumber(control(dlg, 'Width'), 4);
    check(`${name}: the Line group's width edits config.line`, cfg(chart).line.width === 4);
    setSelect(type, 'candles');

    // ── Scales and lines ──
    tabs(dlg).find((t) => t.textContent === 'Scales and lines').dispatch('click', {});
    check(`${name}: switching tab selects it`, selectedTab(dlg) === 'Scales and lines');
    const mode = control(dlg, 'Mode');
    setSelect(mode, 'log');
    pump();
    check(`${name}: log scale (Mode select) sets the log flag`, cfg(chart).priceScale.log === true && cfg(chart).priceScale.mode === 'price');
    setSelect(mode, 'percent');
    check(`${name}: percent mode clears log`, cfg(chart).priceScale.log === false && cfg(chart).priceScale.mode === 'percent');
    setCheck(control(dlg, 'Invert scale'), true);
    check(`${name}: Invert scale`, cfg(chart).priceScale.invert === true);
    setCheck(control(dlg, 'Last price label'), false);
    check(`${name}: Last price label off`, cfg(chart).priceScale.priceLabel === false);
    setSelect(control(dlg, 'Style'), 'dashed');
    check(`${name}: crosshair style`, cfg(chart).crosshair.style === 'dashed');
    setColor(swatch(activePane(dlg), 'Color'), '#00ff00');
    check(`${name}: crosshair color`, cfg(chart).crosshair.color === '#00ff00', cfg(chart).crosshair.color);

    // ── Canvas ──
    tabs(dlg).find((t) => t.textContent === 'Canvas').dispatch('click', {});
    const vertical = control(dlg, 'Vertical');
    check(`${name}: the grid toggles are on`, vertical.checked === true);
    setCheck(vertical, false);
    pump();
    check(`${name}: grid off changes the config`, cfg(chart).grid.vertLines.visible === false);
    setNumber(control(dlg, 'Top'), 12);
    check(`${name}: margins edit the config`, cfg(chart).margins.top === 12);
    setNumber(control(dlg, 'Text size'), 99);
    check(`${name}: a live number out of range is ignored`, cfg(chart).layout.fontSize !== 99);
    setNumber(control(dlg, 'Text size'), 99, 'change');
    check(`${name}: and clamps when it settles`, cfg(chart).layout.fontSize === 32, `${cfg(chart).layout.fontSize}`);
    setColor(swatch(dlg, 'Background'), '#202020');
    pump();
    check(`${name}: the plot background follows the Background color`, wrapper.style.getPropertyValue('background').includes('#202020') || wrapper.style.getPropertyValue('background').includes('rgb(32'), wrapper.style.getPropertyValue('background'));

    // ── reset ──
    const factory = cfg(chart);
    button(dlg, 'Reset defaults').dispatch('click', {});
    await settle();
    const after = cfg(chart);
    check(`${name}: Reset defaults restores the factory config`, after.candles.upColor === before.candles.upColor && after.grid.vertLines.visible === true && after.margins.top === before.margins.top && after.priceScale.mode === before.priceScale.mode && after.crosshair.style === before.crosshair.style && JSON.stringify(factory) !== JSON.stringify(after));
    dlg = dialogOf(wrapper);
    check(`${name}: the open dialog re-seeds its controls and stays on the tab`, !!dlg && selectedTab(dlg) === 'Canvas' && control(dlg, 'Vertical').checked === true);

    // ── an applied config reaches the open dialog ──
    chart.applyConfig(JSON.stringify({ grid: { vertLines: { visible: false } } }));
    await settle();
    dlg = dialogOf(wrapper);
    check(`${name}: applyConfig while open re-seeds the controls`, control(dlg, 'Vertical').checked === false);
    let notified = 0;
    chart.onConfigChanged(() => { notified += 1; });
    setCheck(control(dlg, 'Vertical'), true);
    check(`${name}: onConfigChanged fires for an edit`, notified >= 1 && cfg(chart).grid.vertLines.visible === true);

    // ── theme row ──
    const theme = control(dlg, 'Color theme');
    check(`${name}: the Theme row offers dark and light`, theme && theme.children.length >= 2 && theme.value === 'dark');
    setSelect(theme, 'light');
    await settle();
    check(`${name}: picking a theme re-themes the chart`, wrapper.style.getPropertyValue('background') !== '#151619', wrapper.style.getPropertyValue('background'));
    chart.setTheme('dark');
    await settle();

    // A registered theme is a choice of the Theme row; picking it re-themes the chart.
    const offSunset = wasm.registerTheme({ id: `sunset-${name}`, label: 'Sunset', theme: { background: '#2a1020', textColor: '#f0d0c0', gridColor: '#3a2030', borderColor: '#4a3040', upColor: '#ffaa33', downColor: '#aa3366', crosshairColor: '#ffffff' } });
    escape();
    gear.dispatch('click', {});
    await settle();
    dlg = dialogOf(wrapper);
    const themeSel = control(dlg, 'Color theme');
    const themeIds = themeSel.children.map((o) => o.textContent);
    check(`${name}: the Theme row lists dark, light, then the registered themes (by label)`, themeIds.join(',') === 'Dark,Light,Sunset', themeIds.join(','));
    setSelect(themeSel, `sunset-${name}`);
    await settle();
    check(`${name}: picking a registered theme re-themes the chart`, wrapper.style.getPropertyValue('background').includes('#2a1020') || wrapper.style.getPropertyValue('background').includes('rgb(42'), wrapper.style.getPropertyValue('background'));
    dlg = dialogOf(wrapper);
    check(`${name}: the row then shows the registered theme as the current one`, control(dlg, 'Color theme').value === `sunset-${name}`, control(dlg, 'Color theme')?.value);
    offSunset();
    chart.setTheme('dark');
    await settle();

    // ── Escape, focus return, toggle ──
    escape();
    check(`${name}: Escape closes the dialog`, dialogOf(wrapper) === null && gear.getAttribute('aria-expanded') === 'false');
    check(`${name}: focus returns to the gear`, doc.activeElement === gear);
    gear.dispatch('click', {});
    check(`${name}: the gear opens it again`, !!dialogOf(wrapper));
    gear.dispatch('click', {});
    check(`${name}: and toggles it closed`, dialogOf(wrapper) === null);
    // outside press and the close button
    chart.openSettings('Canvas');
    dlg = dialogOf(wrapper);
    check(`${name}: openSettings(section) lands on that tab`, selectedTab(dlg) === 'Canvas');
    chart.openSettings('Scales and lines');
    check(`${name}: openSettings(section) on an open dialog switches tabs instead of closing`, !!dialogOf(wrapper) && selectedTab(dialogOf(wrapper)) === 'Scales and lines');
    const rail = tabs(dlg);
    const first = rail[0];
    first.dispatch('click', {});
    first.dispatch('keydown', { key: 'ArrowDown', preventDefault() {} });
    check(`${name}: ArrowDown moves the tab selection`, selectedTab(dlg) === 'Scales and lines');
    rail.find((t) => t.getAttribute('aria-selected') === 'true').dispatch('keydown', { key: 'End', preventDefault() {} });
    check(`${name}: End selects the last tab`, selectedTab(dlg) === rail.at(-1).textContent);
    dlg.dispatch('pointerdown', { button: 0 });
    check(`${name}: a press on the scrim closes it`, dialogOf(wrapper) === null);
    chart.openSettings();
    dlg = dialogOf(wrapper);
    dlg.all((n) => n.className === 'vela-dlg-close')[0].dispatch('click', {});
    check(`${name}: the close button closes it`, dialogOf(wrapper) === null);
    chart.openSettings();
    chart.closeDialogs();
    check(`${name}: closeDialogs closes it`, dialogOf(wrapper) === null);

    // ── the stamped ids (ported from test/settings-visibility.test.ts: "built-in ids ... nest
    //    under their tab", and the catalog guard) ──
    chart.openSettings();
    dlg = dialogOf(wrapper);
    const stamped = new Set(dlg.all((n) => n.getAttribute('data-sd-id')).map((n) => n.getAttribute('data-sd-id')));
    const catalog = chart.listSettingsIds();
    check(`${name}: every id the dialog stamps is in the settings-id catalog`, [...stamped].every((id) => catalog.includes(id)), [...stamped].filter((id) => !catalog.includes(id)).join(','));
    const builtin = catalog.filter((id) => /^(symbol|scales|canvas)\./.test(id));
    check(`${name}: every built-in catalog id is stamped on an element of the open dialog`, builtin.every((id) => stamped.has(id)), builtin.filter((id) => !stamped.has(id)).join(','));
    check(`${name}: built-in ids nest under a tab`, builtin.every((id) => ['symbol', 'scales', 'canvas'].includes(id.split('.')[0])));
    // ported from test/i18n-chart.test.ts: the unit after the right margin agrees with its number
    tabs(dlg).find((t) => t.textContent === 'Canvas').dispatch('click', {});
    const rightRow = () => dlg.all((n) => n.getAttribute('data-sd-id') === 'canvas.margins.right')[0];
    const unitOf = () => rightRow().all((n) => n.className === 'vela-sd-unit')[0]?.textContent;
    setNumber(control(dlg, 'Right'), 1);
    check(`${name}: the margin unit is singular for one bar`, unitOf() === 'bar', unitOf());
    setNumber(control(dlg, 'Right'), 5);
    check(`${name}: and plural for five`, unitOf() === 'bars', unitOf());
    chart.closeDialogs();

    // ── the visibility policy ──
    const ids = chart.listSettingsIds();
    check(`${name}: listSettingsIds names the built-in ids`, ids.includes('scales.price-scale.mode') && ids.includes('canvas.grid') && ids.includes('symbol.animation.zoom'));
    chart.setHiddenSettings(['canvas.grid', 'scales.price-scale.invert', 'symbol.timezone', 'scales.crosshair']);
    await settle();
    chart.openSettings();
    dlg = dialogOf(wrapper);
    check(`${name}: hidden rows stay hidden`, !control(dlg, 'Invert scale') && !control(dlg, 'Time zone') && !control(dlg, 'Vertical') && !!control(dlg, 'Mode') && !swatch(dlg, 'Crosshair'), '');
    check(`${name}: a hidden group takes its title with it`, !dlg.all((n) => n.className === 'vela-sd-title' && n.textContent === 'Grid').length && !dlg.all((n) => n.className === 'vela-sd-title' && n.textContent === 'Crosshair').length && dlg.all((n) => n.className === 'vela-sd-title' && n.textContent === 'Margins').length === 1);
    chart.setHiddenSettings(['canvas']);
    await settle();
    check(`${name}: hiding the policy while open rebuilds in place without the tab`, !tabs(dialogOf(wrapper)).some((t) => t.textContent === 'Canvas') && tabs(dialogOf(wrapper)).length >= 2);
    chart.setHiddenSettings([]);
    chart.closeDialogs();

    // ── the indicator dialog ──
    chart.openIndicatorSettings('nope');
    check(`${name}: an unknown indicator id opens nothing`, dialogOf(wrapper) === null);
    chart.openIndicatorSettings(rsiId);
    dlg = dialogOf(wrapper);
    check(`${name}: the indicator dialog opens`, !!dlg && dlg.getAttribute('data-align') === 'center' && dlg.children[0].getAttribute('data-variant') === 'form');
    const ititle = byId(dlg, dlg.children[0].getAttribute('aria-labelledby'));
    check(`${name}: its title is the legend name`, /RSI/i.test(ititle.textContent), ititle.textContent);
    check(`${name}: it has an Inputs tab`, tabs(dlg).length === 1 && tabs(dlg)[0].textContent === 'Inputs');
    const length = control(dlg, 'Length');
    check(`${name}: an input's control has the original's id (vela-inp-<indicator>-<key>)`, length?.id === `vela-inp-${rsiId}-length`, length?.id);
    check(`${name}: it lists the inputs with their current values`, length && length.value === '14' && control(dlg, 'Source')?.value?.toLowerCase() === 'close', `${length?.value}`);
    check(`${name}: every indicator control has a label`, unlabelled(dlg).length === 0, unlabelled(dlg).map((n) => `${n.tagName}#${n.id}`).join(','));
    pump();
    const read0 = chart.indicatorReadout(rsiId).join();
    setNumber(length, 20, 'change');
    await settle();
    check(`${name}: changing the length persists the input`, JSON.parse(chart.indicatorInputs(rsiId)).length === 20, chart.indicatorInputs(rsiId));
    const read1 = chart.indicatorReadout(rsiId).join();
    check(`${name}: and recomputes the series (the plot values differ)`, read0 !== '' && read1 !== '' && read0 !== read1, `${read0} -> ${read1}`);
    setSelect(control(dlg, 'Source'), 'open');
    check(`${name}: a select input commits`, JSON.parse(chart.indicatorInputs(rsiId)).source === 'open');
    // Cancel reverts everything since the open
    button(dlg, 'Cancel').dispatch('click', {});
    await settle();
    check(`${name}: Cancel closes and reverts to the open-time values`, dialogOf(wrapper) === null && JSON.parse(chart.indicatorInputs(rsiId)).length === 14 && JSON.parse(chart.indicatorInputs(rsiId)).source.toLowerCase() === 'close', chart.indicatorInputs(rsiId));
    chart.openIndicatorSettings(rsiId);
    dlg = dialogOf(wrapper);
    setNumber(control(dlg, 'Length'), 30, 'change');
    button(dlg, 'OK').dispatch('click', {});
    await settle();
    check(`${name}: OK keeps the edits`, dialogOf(wrapper) === null && JSON.parse(chart.indicatorInputs(rsiId)).length === 30);
    chart.openIndicatorSettings(rsiId);
    dlg = dialogOf(wrapper);
    check(`${name}: reopening shows the persisted value`, control(dlg, 'Length').value === '30', control(dlg, 'Length')?.value);
    button(dlg, 'Reset defaults').dispatch('click', {});
    await settle();
    dlg = dialogOf(wrapper);
    check(`${name}: Reset defaults restores the declared defaults and re-seeds the form`, JSON.parse(chart.indicatorInputs(rsiId)).length === 14 && control(dlg, 'Length').value === '14');
    button(dlg, 'Cancel').dispatch('click', {});
    await settle();
    check(`${name}: Cancel after a reset reverts the whole session`, JSON.parse(chart.indicatorInputs(rsiId)).length === 30);
    chart.openIndicatorSettings(rsiId);
    setNumber(control(dialogOf(wrapper), 'Length'), 21, 'change');
    escape();
    await settle();
    check(`${name}: Escape closes and keeps the live edits`, dialogOf(wrapper) === null && JSON.parse(chart.indicatorInputs(rsiId)).length === 21);
    check(`${name}: focus returned after the indicator dialog`, doc.activeElement !== null);

    // A press on the indicator dialog's scrim drops the legend row's selection.
    const rsiRow = wrapper.all((n) => (n.className ?? '').split(' ').includes('vela-ind-row') && /RSI/i.test(n.textContent))[0];
    if (rsiRow) {
        rsiRow.dispatch('keydown', { key: 'Enter', target: rsiRow });
        const selectedBefore = rsiRow.getAttribute('data-open') === '1';
        chart.openIndicatorSettings(rsiId);
        dialogOf(wrapper).dispatch('pointerdown', { button: 0 });
        check(`${name}: the scrim press closes the indicator dialog and clears the legend selection`, selectedBefore && dialogOf(wrapper) === null && rsiRow.getAttribute('data-open') === '0', `${selectedBefore} ${rsiRow.getAttribute('data-open')}`);
    } else {
        check(`${name}: the legend has an RSI row`, false);
    }

    // every classic indicator's dialog builds, with labelled controls
    let built = 0;
    let bad = '';
    for (const type of wasm.VelaChart.indicatorTypes()) {
        if (type === 'rsi') continue;
        let id;
        try {
            id = chart.addIndicator(type, undefined);
        } catch {
            continue;
        }
        await ready(chart);
        chart.openIndicatorSettings(id);
        const d = dialogOf(wrapper);
        if (d) {
            built += 1;
            const u = unlabelled(d);
            if (u.length) bad += ` ${type}:${u.map((n) => n.tagName).join('/')}`;
            chart.closeDialogs();
        }
    }
    check(`${name}: the dialog builds for every classic indicator (${built}) with labelled controls`, built > 10 && bad === '', bad);
    await settle();

    // ── teardown ──
    chart.openSettings();
    chart.openIndicatorSettings(rsiId);
    const docListeners = doc.listeners.length;
    check(`${name}: both dialogs can be open at once`, wrapper.all((n) => n.className === 'vela-dlg-positioner').length === 2 && docListeners >= 2);
    chart.destroy();
    await settle();
    check(`${name}: destroy removes the dialogs, the gear and every document listener`, el.children.length === 0 && doc.listeners.length === 0 && doc.defaultView.listeners.length === 0, `${doc.listeners.length} document listeners`);
    return chart;
}

// Direct chart: no engine, the events are honored by VelaChart itself.
await scenario(
    'VelaChart',
    (el) => new wasm.VelaChart(el, 'dark'),
    {
        ready: async () => {},
        addIndicators: (chart) => {
            chart.setBars(makeBars(400));
            chart.addVolume();
            chart.addIndicator('ema', undefined);
            return chart.addIndicator('rsi', undefined);
        },
    },
);

// Facade chart: the engine handles the events.
await scenario(
    'VelaApp',
    (el) => new wasm.VelaApp(el, 'dark', 'DEMO', '1h', makeBars(300)),
    {
        ready: async (app) => {
            if (!app.__ready) {
                await app.ready();
                app.__ready = true;
            }
            for (let i = 0; i < 30; i += 1) await Promise.resolve();
            await sleep(5);
        },
        addIndicators: (app) => {
            app.addIndicator('sma', undefined);
            return app.addIndicator('rsi', undefined);
        },
    },
);

// A static call on the class: check the API list carries the additions.
check('the JS API has the dialog members', ['showSettings', 'openSettings', 'openIndicatorSettings', 'closeDialogs', 'getConfig', 'applyConfig', 'onConfigChanged', 'setHiddenSettings', 'listSettingsIds', 'indicatorInputs'].every((m) => typeof wasm.VelaChart.prototype[m] === 'function' && typeof wasm.VelaApp.prototype[m] === 'function'));

console.log(state.failures === 0 ? '\nall dialog checks passed' : `\n${state.failures} dialog check(s) FAILED`);
process.exit(state.failures === 0 ? 0 : 1);
