// Smoke test of vela-web's `Vela` class (src/vela_api): the original's public chart API for
// JavaScript, in Node against the minimal fake DOM.
//
// It constructs the chart the way the original's quick start does, calls a representative
// method of every control surface and asserts its effect, fires every chart event it can and
// checks the payload shape, drives a data provider and a data feed written in JavaScript
// (promises, a rejected promise, a DataUnavailableError), checks the misuse errors and the
// teardown. It does NOT run a browser: real layout, real painting and real pointer capture are
// not exercised, and no exchange endpoint is ever called (the bundled exchange providers are
// constructed, never registered).
//
// Four events cannot fire in this build and are only checked to subscribe: `script:run`,
// `context:changed`, `alert` and `warning` come from a scripting engine, and none can be
// registered.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_vela_api.mjs pkg
//
// Exits 1 on the first failed check.

import { FakeCanvas, check, clock, container, doc, frames, loadPackage, makeBars, observers, painted, pileOf, pump, state, dprWatchers } from './fake_dom.mjs';

FakeCanvas.prototype.toDataURL = function toDataURL(type) {
    return `data:${type};size=${this.width}x${this.height}`;
};
const warnings = [];
const realWarn = console.warn;
console.warn = (...args) => {
    warnings.push(args.map(String).join(' '));
};

const wasm = await loadPackage('smoke_vela_api.mjs');
const { Vela } = wasm;

// ── helpers ──

const macrotask = (ms = 4) => new Promise((resolve) => setTimeout(resolve, ms));
async function settle(rounds = 4) {
    for (let i = 0; i < rounds; i += 1) {
        for (let j = 0; j < 6; j += 1) {
            pump();
            await Promise.resolve();
        }
        await macrotask();
    }
    pump();
}
async function until(predicate, what, ms = 3000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
        if (predicate()) return true;
        await settle(1);
    }
    check(`timeout waiting for ${what}`, false);
    return false;
}
const throws = (fn, pattern, type) => {
    try {
        fn();
    } catch (e) {
        const text = String(e?.message ?? e);
        return pattern.test(text) && (type === undefined || e?.constructor?.name === type || e?.name === type);
    }
    return false;
};
const rejects = async (promise, pattern) => {
    try {
        await promise;
    } catch (e) {
        return pattern.test(String(e?.message ?? e));
    }
    return false;
};
const has = (o, ...keys) => keys.every((k) => k in o);
const isNum = (v) => typeof v === 'number' && Number.isFinite(v);
const timers = () => process.getActiveResourcesInfo().filter((r) => r === 'Timeout').length;
const totalListeners = (node) => node.listeners.length + node.children.reduce((n, c) => n + totalListeners(c), 0);

const ALL_EVENTS = [
    'ready', 'market:changed', 'load:start', 'load:end', 'indicator:added', 'indicator:removed', 'indicator:inputs', 'indicator:error', 'data:unresolved',
    'data:unavailable', 'symbol:facts', 'indicator:moved', 'indicator:visibility', 'pane:changed', 'theme:changed', 'locale:changed', 'priceStyle:change',
    'pane:moved', 'drawing:created', 'drawing:draft', 'drawing:edited', 'drawing:selected', 'drawing:favorites', 'drawing:defaults', 'drawing:templates',
    'drawing:tool', 'drawing:snap', 'drawing:stay', 'drawing:mode', 'drawing:removed', 'drawing:settings', 'mark:click', 'script:run', 'context:changed', 'bar',
    'viewport:changed', 'history:progress', 'history:complete', 'replay:start', 'replay:step', 'replay:tick', 'replay:play', 'replay:pause', 'replay:end', 'alert', 'warning',
];
const NEVER_FIRE = ['script:run', 'context:changed', 'alert', 'warning'];
check('the original declares 46 chart events', ALL_EVENTS.length === 46 && new Set(ALL_EVENTS).size === 46);

// Every payload shape the original's `VelaEventMap` documents.
const obj = (v) => v !== null && typeof v === 'object';
const str = (v) => typeof v === 'string';
const strOrNull = (v) => v === null || typeof v === 'string';
const SHAPES = {
    ready: (p) => p === undefined,
    'market:changed': (p) => str(p.symbol) && str(p.timeframe) && obj(p.prev) && str(p.prev.symbol) && str(p.prev.timeframe),
    'load:start': (p) => str(p.symbol) && str(p.timeframe) && typeof p.firstLoad === 'boolean',
    'load:end': (p) => str(p.symbol) && str(p.timeframe) && isNum(p.bars),
    'indicator:added': (p) => str(p.id),
    'indicator:removed': (p) => str(p.id),
    'indicator:inputs': (p) => str(p.id),
    'indicator:error': (p) => str(p.id) && p.error instanceof Error,
    'data:unresolved': (p) => str(p.symbol) && Array.isArray(p.providers),
    'data:unavailable': (p) => str(p.symbol) && str(p.timeframe) && p.reason === 'timeframe' && p.finest === '1D' && p.default === '1D',
    'symbol:facts': (p) => str(p.symbol) && (p.facts === null || obj(p.facts)),
    'indicator:moved': (p) => str(p.id) && str(p.paneId),
    'indicator:visibility': (p) => str(p.id) && typeof p.visible === 'boolean',
    'pane:changed': (p) => p === undefined,
    'theme:changed': (p) => obj(p) && str(p.background) && str(p.upColor) && str(p.fontFamily),
    'locale:changed': (p) => str(p.locale) && (p.direction === 'ltr' || p.direction === 'rtl'),
    'priceStyle:change': (p) => str(p.from) && str(p.to) && p.from !== p.to,
    'pane:moved': (p) => str(p.paneId) && (p.dir === 'up' || p.dir === 'down'),
    'drawing:created': (p) => str(p.id),
    'drawing:draft': (p) => p.doc === null || (obj(p.doc) && str(p.doc.type)),
    'drawing:edited': (p) => str(p.id),
    'drawing:selected': (p) => strOrNull(p.id) && Array.isArray(p.ids),
    'drawing:favorites': (p) => Array.isArray(p.favorites),
    'drawing:defaults': (p) => str(p.type),
    'drawing:templates': (p) => str(p.type),
    'drawing:tool': (p) => strOrNull(p.type),
    'drawing:snap': (p) => ['off', 'weak', 'strong'].includes(p.mode),
    'drawing:stay': (p) => typeof p.on === 'boolean',
    'drawing:mode': (p) => p.mode === null || p.mode === 'measure' || p.mode === 'eraser',
    'drawing:removed': (p) => str(p.id),
    'drawing:settings': (p) => str(p.id),
    'mark:click': (p) => str(p.id) && Array.isArray(p.ids) && p.ids.length >= 1 && isNum(p.time),
    bar: (p) => isNum(p.time) && isNum(p.open) && isNum(p.high) && isNum(p.low) && isNum(p.close),
    'viewport:changed': (p) => isNum(p.from) && isNum(p.to) && p.from <= p.to,
    'history:progress': (p) => isNum(p.loaded) && isNum(p.target) && p.loaded <= p.target,
    'history:complete': (p) => ['depth', 'genesis', 'aborted'].includes(p.reason) && isNum(p.oldestTime) && isNum(p.barsLoaded),
    'replay:start': (p) => isNum(p.cursorTime) && isNum(p.remaining),
    'replay:step': (p) => isNum(p.cursorTime) && isNum(p.remaining),
    'replay:tick': (p) => isNum(p.cursorTime) && isNum(p.index) && isNum(p.count) && p.index >= 1 && p.index <= p.count,
    'replay:play': (p) => isNum(p.intervalMs),
    'replay:pause': (p) => p === undefined,
    'replay:end': (p) => ['stopped', 'finished', 'market'].includes(p.reason),
};
const fired = new Map(); // event name -> payloads, over every chart of this script
const shapeFailures = [];
function watchAll(chart, label) {
    const offs = [];
    for (const name of ALL_EVENTS) {
        offs.push(
            chart.on(name, (payload) => {
                if (!fired.has(name)) fired.set(name, []);
                fired.get(name).push(payload);
                let ok = false;
                try {
                    ok = SHAPES[name](payload);
                } catch {
                    ok = false;
                }
                if (!ok) shapeFailures.push(`${label}: ${name} ${JSON.stringify(payload)}`);
            }),
        );
    }
    return offs;
}
const count = (name) => (fired.get(name) ?? []).length;
const last = (name) => (fired.get(name) ?? []).at(-1);

// A deterministic bar generator over a fixed time grid: bar k opens at T0 + k * step.
const T0 = Date.UTC(2024, 0, 1);
function gridBars(step, total, range = {}) {
    const out = [];
    for (let k = 0; k < total; k += 1) {
        const time = T0 + k * step;
        if (range.from !== undefined && time < range.from) continue;
        if (range.to !== undefined && time > range.to) continue;
        const base = 100 + 10 * Math.sin(k / 40) + k * 0.002;
        out.push({ time, open: base, high: base + 1.5, low: base - 1.5, close: base + 0.5 * Math.cos(k / 7), volume: 500 + (k % 50) * 10 });
    }
    return range.limit !== undefined ? out.slice(-range.limit) : out;
}
const HOUR = 3_600_000;
const data = makeBars(300);

// ── 1. the surface: every member of the original's `Vela`, and its control classes ──

const VELA_MEMBERS = [
    'i18n', 'setLocale', 'registerEngine', 'addIndicator', 'addNativeIndicator', 'availableNativeIndicators', 'presentNativeIndicators', 'pendingNativeIndicators',
    'symbolFacts', 'indicators', 'runScript', 'runIndicator', 'setMarket', 'market', 'ready', 'historyComplete', 'inspect', 'renderer', 'panes', 'data', 'drawings',
    'marks', 'replay', 'on', 'getVisibleRange', 'setVisibleRange', 'panBy', 'setVisibleRangePreset', 'resize', 'setTheme', 'destroy',
];
check('Vela has every member of the original class', VELA_MEMBERS.every((m) => m in Vela.prototype), VELA_MEMBERS.filter((m) => !(m in Vela.prototype)).join(','));
const EXPORTS = ['Vela', 'DataControl', 'PanesControl', 'ReplayControl', 'RendererControl', 'DrawingsControl', 'MarksControl', 'IndicatorHandle', 'DataUnavailableError', 'isDataUnavailableError', 'BinanceProvider', 'CoinbaseProvider', 'HyperliquidProvider', 'lowerTimeframeTicks', 'barsToTicks', 'registerLocale', 'registerTheme', 'registerWidgetAction', 'registerWidgetAttachment', 'registerSidePanel', 'registerLegendAction', 'registerLegendCallout', 'registerStatePersistence', 'registerSymbolRanking', 'registerShellDefaults', 'VelaApp', 'VelaChart', 'VelaWorkspace', 'VelaWidget'];
check('the package exports the original names', EXPORTS.every((n) => n in wasm), EXPORTS.filter((n) => !(n in wasm)).join(','));

// ── 2. construction: the quick start ──

const timersBefore = timers();
const observersBefore = observers.length;
const host = container(900, 560);
const listenersBefore = totalListeners(host);
const docListenersBefore = doc.listeners.length;
const chart = new Vela(host, { data, timeframe: '1h', theme: 'dark' });
const offs = watchAll(chart, 'main');
check('the constructor takes (container, options) and builds the chart inside the container', chart instanceof Vela && host.children.length === 1);
await chart.ready();
await settle();
const pile = pileOf(host);
const dataCanvas = pile.canvases.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
check('the bars are loaded and painted', pile.canvases.length === 7 && painted(dataCanvas) && painted(pile.canvases[4]), `${pile.canvases.length} canvases`);
check('market reads the offline market', JSON.stringify(chart.market) === JSON.stringify({ timeframe: '1h', offline: true }), JSON.stringify(chart.market));
const bounds = chart.replay.bounds;
check('replay.bounds spans the loaded bars', bounds && bounds.first === data[0].time && bounds.last === data.at(-1).time);
const range0 = chart.getVisibleRange();
check('getVisibleRange reads real bar times', range0 && range0.from >= data[0].time && range0.to <= data.at(-1).time, JSON.stringify(range0));
check('ready fired and history:complete says depth or genesis', count('ready') === 1 && ['depth', 'genesis'].includes(last('history:complete')?.reason));
check('the volume indicator is added on its own (volume: true by default)', chart.presentNativeIndicators().includes('volume') && count('indicator:added') >= 1);
check('the default drawing toolbar is shown, as in the original', pile.canvases.length === 7 && dataCanvas.width < 900, `data canvas ${dataCanvas.width}px`);
check('controls are the same object on every read', chart.data === chart.data && chart.panes === chart.panes && chart.replay === chart.replay && chart.renderer === chart.renderer && chart.drawings === chart.drawings && chart.marks === chart.marks && chart.i18n === chart.i18n);
check('control classes carry the original names', chart.data instanceof wasm.DataControl && chart.renderer instanceof wasm.RendererControl && chart.drawings instanceof wasm.DrawingsControl && chart.marks instanceof wasm.MarksControl && chart.replay instanceof wasm.ReplayControl && chart.panes instanceof wasm.PanesControl);
const CONTROL_MEMBERS = {
    data: ['registerProvider', 'unregisterProvider', 'providers', 'resolve', 'displayPrefix', 'canonicalSymbol', 'providerInstance', 'symbols', 'symbolIcon', 'symbolInfo', 'capabilities', 'ready'],
    panes: ['supported', 'list', 'move', 'remove', 'collapse', 'maximize', 'setHeightWeight', 'moveIndicator'],
    replay: ['start', 'step', 'stepUpdate', 'play', 'pause', 'stop', 'state', 'bounds', 'setTicks'],
    renderer: ['name', 'capabilities', 'supports', 'get', 'set', 'screenshot', 'getConfig', 'applyConfig', 'onConfigChanged', 'onCrosshairMove', 'onClick', 'onAxisLongPress', 'dataWindowReadout', 'setExternalCrosshair', 'supportsExternalCrosshair', 'focus', 'closeDialogs', 'openSettings', 'openIndicatorSettings', 'supportsIndicatorSettings', 'setSettingsVisibility', 'listSettingsIds', 'setLayoutMode'],
    drawings: ['supported', 'setTool', 'getTool', 'setSnapMode', 'getSnapMode', 'setStayMode', 'getStayMode', 'setMode', 'getMode', 'showToolbar', 'setToolbar', 'setToolShortcuts', 'setExternalGhost', 'add', 'remove', 'update', 'updateMany', 'removeMany', 'lock', 'show', 'select', 'openSettings', 'bringToFront', 'sendToBack', 'undo', 'redo', 'canUndo', 'canRedo', 'clone', 'duplicate', 'copyToClipboard', 'paste', 'all', 'toJSON', 'fromJSON', 'favorites', 'isFavorite', 'setFavorite', 'setFavorites', 'toolDefaults', 'setToolDefaults', 'resetToolDefaults', 'toolTemplates', 'setToolTemplates', 'getConfig', 'applyConfig'],
    marks: ['supported', 'add', 'set', 'remove', 'clear', 'all', 'defineGroup', 'groups', 'setGroupVisible', 'isGroupVisible'],
};
for (const [name, members] of Object.entries(CONTROL_MEMBERS)) {
    const missing = members.filter((m) => !(m in chart[name]));
    check(`chart.${name} has every member of the original`, missing.length === 0, missing.join(','));
}

// ── 3. the selector form, misuse errors and what the constructor refuses ──

const selectorHost = container(500, 300);
doc.querySelector = (sel) => (sel === '#chart' ? selectorHost : null);
const bySelector = new Vela('#chart', { data: makeBars(50), timeframe: '1h' });
check('a CSS selector string finds the container', selectorHost.children.length === 1);
bySelector.destroy();
check('a missing selector throws the original message', throws(() => new Vela('#nope', { data }), /^\[vela\] container not found for selector "#nope"$/));
check('a container that is neither an element nor a selector is a TypeError', throws(() => new Vela(42, { data }), /container must be an HTMLElement or a CSS selector/, 'TypeError'));
check('options must be an object', throws(() => new Vela(container(100, 100), 'dark'), /options\) must be an object/, 'TypeError'));
const refusal = (what, make) => throws(make, new RegExp(`\\[vela\\] ${what.replace(/[.()]/g, '\\$&')} is not supported by this build`));
check('options.renderer (a renderer class) is refused with a clear error', refusal('options.renderer', () => new Vela(container(100, 100), { data, renderer: class {} })));
check('deps.renderer is refused', refusal('deps.renderer', () => new Vela(container(100, 100), { data }, { renderer: {} })));
check('deps.engines is refused', refusal('deps.engines', () => new Vela(container(100, 100), { data }, { engines: [{ language: 'pine' }] })));
check('an empty deps.engines list is accepted', (() => {
    const c = new Vela(container(100, 100), { data }, { engines: [] });
    c.destroy();
    return true;
})());
check('options.formatters is refused', refusal('options.formatters', () => new Vela(container(100, 100), { data, formatters: { price: () => '' } })));
check('options.priceAxis.ticks is refused', refusal('options.priceAxis.ticks', () => new Vela(container(100, 100), { data, priceAxis: { ticks: () => [] } })));
check('a freePan predicate is refused', refusal('options.freePan as a function', () => new Vela(container(100, 100), { data, freePan: () => true })));
check('a bad nativeBackend is a TypeError', throws(() => new Vela(container(100, 100), { data, nativeBackend: 'gl' }), /nativeBackend must be/, 'TypeError'));
check('a bad bar in options.data names the field', throws(() => new Vela(container(100, 100), { data: [{ time: 1, open: 1, high: 'x', low: 1, close: 1 }] }), /bar\.high must be a finite number/));
check('an unknown provider name in options.providers is an error', throws(() => new Vela(container(100, 100), { data, providers: ['nope'] }), /unknown provider "nope"/));
check('a refused construction leaves no DOM behind', (() => {
    const h = container(100, 100);
    throws(() => new Vela(h, { data, renderer: class {} }), /renderer/);
    return h.children.length === 0;
})());
check('registerEngine is refused', throws(() => chart.registerEngine('pine', {}), /registerEngine\(pine\) is not supported by this build/));
check('on() needs a function', throws(() => chart.on('bar', 5), /handler must be a function/, 'TypeError'));
check('an unknown event name registers nothing and never fires, as in the original', typeof chart.on('no:such:event', () => {}) === 'function');
check('setVisibleRangePreset rejects an unknown preset', throws(() => chart.setVisibleRangePreset('2W'), /unknown preset "2W"/, 'TypeError'));
check('setVisibleRange needs { from, to }', throws(() => chart.setVisibleRange({ from: 1 }), /setVisibleRange takes \{ from, to \}/, 'TypeError'));
check('setMarket needs an object', throws(() => chart.setMarket('BTC'), /setMarket takes an object/, 'TypeError'));
check('setMarket rejects a bad session by ignoring it, a bad bars count by erroring', throws(() => chart.setMarket({ bars: -5 }), /`bars` must be a positive number/, 'TypeError'));
check('setLocale needs a tag or an object', throws(() => chart.setLocale(7), /setLocale takes a locale tag or an object/, 'TypeError'));
check('setLocale refuses a formatters function', throws(() => chart.setLocale({ formatters: {} }), /options.formatters is not supported/));
check('panes.move rejects a bad direction', throws(() => chart.panes.move('p', 'left'), /dir must be "up" or "down"/, 'TypeError'));
check('replay.start needs { from }', throws(() => chart.replay.start({}), /`from` must be epoch-ms/, 'TypeError'));
check('replay.setTicks takes a function or null', throws(() => chart.replay.setTicks(5), /function or null/, 'TypeError'));
check('renderer.set refuses a function value', throws(() => chart.renderer.set('priceAxisTicks', () => []), /function value is not supported/));
check('drawings.setSnapMode rejects an unknown mode', throws(() => chart.drawings.setSnapMode('huge'), /"off", "weak" or "strong"/, 'TypeError'));
check('drawings.setMode rejects an unknown mode', throws(() => chart.drawings.setMode('lasso'), /"measure", "eraser" or null/, 'TypeError'));
check('drawings.add rejects anchors that are not points', throws(() => chart.drawings.add('hline', { anchors: [1, 2] }), /init\.anchors/, 'TypeError'));
check('marks.add needs a numeric time and a glyph', throws(() => chart.marks.add({ id: 'x', glyph: { color: '#fff' } }), /`time` must be epoch-ms/, 'TypeError') && throws(() => chart.marks.add({ id: 'x', time: 1 }), /needs a `glyph`/, 'TypeError'));
check('a mark with an empty id is refused by the model', throws(() => chart.marks.add({ id: '', time: 1, glyph: { color: '#fff', letter: 'A' } }), /`id` must be a non-empty string/));
check('a mark with a lazy content function is refused', throws(() => chart.marks.add({ id: 'lazy', time: 1, glyph: { color: '#fff' }, content: () => ({ text: 'x' }) }), /lazy content\) is not supported/));
check('registerProvider needs a name and a getBars method', throws(() => chart.data.registerProvider('x', {}), /must implement getBars/, 'TypeError') && throws(() => chart.data.registerProvider(5, {}), /`name` must be a string/, 'TypeError'));
check('addIndicator needs a string source', throws(() => chart.addIndicator(5), /`source` must be a string/, 'TypeError'));
check('a duplicate indicator id is an error', (() => {
    const h = chart.addIndicator('//noop', { id: 'dup-1' });
    const second = throws(() => chart.addIndicator('//noop', { id: 'dup-1' }), /already live|dup-1/);
    h.remove();
    return second;
})());
check('an empty indicator id is an error', throws(() => chart.addIndicator('//noop', { id: '' }), /id/));
await settle();

// ── 4. indicators: native add, inputs, visibility, removal; a script without an engine ──

const added0 = count('indicator:added');
const sma = chart.addNativeIndicator('moving-average', { inputs: { length: 12 } });
await until(() => count('indicator:added') > added0, 'indicator:added');
check('addNativeIndicator returns a handle that carries the original members', sma instanceof wasm.IndicatorHandle && sma.nativeType === 'moving-average' && typeof sma.id === 'string' && sma.title.length > 0 && sma.source === undefined && sma.language === undefined && sma.visible === true);
const lengthInput = sma.inputs.find((i) => i.key === 'length');
check('handle.inputs lists the schema with key, title, type and defval', lengthInput && lengthInput.type === 'int' && isNum(lengthInput.defval) && str(lengthInput.title) && sma.inputs.every((i) => has(i, 'key', 'title', 'type', 'defval')));
check('indicator:added carries the id', last('indicator:added').id === sma.id);
check('indicators() returns the same handle objects', chart.indicators().includes(sma) && chart.presentNativeIndicators().filter((t) => t === 'moving-average').length === 1);
const inputsBefore = count('indicator:inputs');
check('setInput changes one input and chains', sma.setInput('length', 30) === sma);
check('indicator:inputs fires with the id', count('indicator:inputs') === inputsBefore + 1 && last('indicator:inputs').id === sma.id);
sma.setInputs({ length: 8 });
check('setInputs changes several inputs', count('indicator:inputs') === inputsBefore + 2);
check('a bad input value is a TypeError', throws(() => sma.setInput('length', {}), /number, a string or a boolean/, 'TypeError'));
sma.setVisible(false);
check('setVisible(false) hides and fires indicator:visibility', sma.visible === false && last('indicator:visibility')?.visible === false && last('indicator:visibility')?.id === sma.id);
sma.setVisible(true);
check('setVisible(true) shows again', sma.visible === true && last('indicator:visibility').visible === true);
const second = chart.addNativeIndicator('moving-average');
check('a classic study allows several instances', second.id !== sma.id && chart.presentNativeIndicators().filter((t) => t === 'moving-average').length === 2);
check('a layer-backed native type is single-instance', chart.addNativeIndicator('volume').id === chart.indicators().find((h) => h.nativeType === 'volume').id);
const catalog = await chart.availableNativeIndicators();
check('availableNativeIndicators lists the catalog with its live state', catalog.length > 10 && catalog.every((c) => has(c, 'type', 'title', 'supported', 'present', 'multiInstance')) && catalog.find((c) => c.type === 'moving-average').present === true && catalog.find((c) => c.type === 'moving-average').multiInstance === true);
const removedBefore = count('indicator:removed');
second.remove();
check('handle.remove fires indicator:removed', count('indicator:removed') === removedBefore + 1 && !chart.indicators().includes(second));
const nothing = chart.addNativeIndicator('no-such-indicator');
check('an unregistered native type returns a handle that never mounts, with a warning', nothing instanceof wasm.IndicatorHandle && warnings.some((w) => /no native indicator registered/.test(w)));
nothing.remove();
// A script needs an engine: none can be registered, so it reports through the handle.
const scriptErrors = [];
const scriptHandle = chart.addIndicator('//@version=5\nindicator("x")\nplot(close)', { title: 'Script' });
scriptHandle.on('error', (p) => scriptErrors.push(p));
check('addIndicator(source) returns a handle with the source and the default language', scriptHandle.source?.includes('indicator("x")') && scriptHandle.language === 'pine' && scriptHandle.nativeType === undefined && scriptHandle.title === 'Script');
await until(() => scriptErrors.length > 0 || count('indicator:error') > 0, 'the engine error');
check('without an engine the failure arrives as the handle error event and indicator:error', (scriptErrors[0]?.error instanceof Error || count('indicator:error') > 0) && (last('indicator:error') === undefined || last('indicator:error').id === scriptHandle.id), scriptErrors[0]?.error?.message);
scriptHandle.remove();
const run = await chart.runIndicator('//@version=5\nindicator("y")\nplot(close)');
check('runIndicator resolves ok:false with an Error and no handle, never rejecting', run.ok === false && run.handle === null && run.error instanceof Error && 'context' in run, run.error?.message);
const runScript = await chart.runScript('//@version=5\nindicator("z")\nplot(close)');
check('runScript resolves ok:false with onUpdate and remove callable', runScript.ok === false && runScript.run === null && runScript.error instanceof Error && typeof runScript.onUpdate(() => {}) === 'function' && runScript.remove() === undefined);
await settle();
check('a failed script leaves no legend row behind', !chart.indicators().some((h) => h.source !== undefined));
check('handle.updateCode on a native indicator resolves ok:false', (await sma.updateCode('x')).ok === false);
check('handle.context() resolves null for a native indicator', (await sma.context()) === null);
check('inspect() counts what the core generated, in the original shape', (() => {
    const s = chart.inspect();
    return Array.isArray(s.indicators) && s.indicators.some((i) => i.nativeType === 'moving-average' && i.native === true && typeof i.series === 'object') && isNum(s.totals.panes) && has(s.totals, 'series', 'fills', 'priceLines', 'forcedOverlay');
})());

// ── 5. panes: the second pane, order, collapse, maximize, weights ──

const rsi = chart.addNativeIndicator('rsi');
const macd = chart.addNativeIndicator('macd');
await until(() => chart.panes.list().length >= 3, 'two study panes');
const paneChanged0 = count('pane:changed');
let panes = chart.panes.list();
const paneOf = (handle) => chart.panes.list().find((p) => p.indicators.some((i) => i.id === handle.id));
check('panes.supported and list() report the panes top to bottom', chart.panes.supported === true && panes[0].kind === 'price' && panes[0].id === 'price' && panes.slice(1).every((p) => p.kind === 'study') && panes.every((p, i) => p.order === i && typeof p.collapsed === 'boolean' && typeof p.maximized === 'boolean' && Array.isArray(p.indicators)), JSON.stringify(panes.map((p) => [p.id, p.order])));
check('a pane lists its indicators with id, title and ownScale', paneOf(rsi)?.indicators.every((i) => has(i, 'id', 'title', 'ownScale')));
const rsiPane = paneOf(rsi).id;
const macdPane = paneOf(macd).id;
check('a study pane starts at height weight 1 and the price pane at 3', panes[0].heightWeight === 3 && paneOf(rsi).heightWeight === 1);
chart.panes.move(macdPane, 'up');
await settle();
check('panes.move reorders and fires pane:moved and pane:changed', chart.panes.list().findIndex((p) => p.id === macdPane) < chart.panes.list().findIndex((p) => p.id === rsiPane) && last('pane:moved')?.paneId === macdPane && last('pane:moved')?.dir === 'up' && count('pane:changed') > paneChanged0);
check('panes calls chain', chart.panes.collapse(rsiPane) === chart.panes);
check('panes.collapse collapses the pane', paneOf(rsi).collapsed === true);
chart.panes.collapse(rsiPane, false);
check('collapse(id, false) restores it', paneOf(rsi).collapsed === false);
chart.panes.maximize(macdPane);
check('panes.maximize lets one pane fill the plot', chart.panes.list().find((p) => p.id === macdPane).maximized === true);
chart.panes.maximize(null);
check('maximize(null) restores the split', chart.panes.list().every((p) => !p.maximized));
chart.panes.setHeightWeight('price', 6);
check('setHeightWeight sets the weight', chart.panes.list()[0].heightWeight === 6);
const warnsBefore = warnings.length;
chart.panes.setHeightWeight('price', -1);
check('a non-positive weight warns and keeps the weight', warnings.length > warnsBefore && chart.panes.list()[0].heightWeight === 6);
rsi.moveTo({ newPane: true });
await settle();
check('handle.moveTo({ newPane }) moves into a pane of its own and fires indicator:moved', last('indicator:moved')?.id === rsi.id && str(last('indicator:moved').paneId));
chart.panes.moveIndicator(macd.id, { pane: paneOf(rsi).id });
await settle();
check('panes.moveIndicator merges an indicator into a pane', paneOf(macd)?.id === paneOf(rsi)?.id && paneOf(rsi).indicators.length === 2);
macd.moveTo('price');
await settle();
check('moveTo("price") merges back onto the price pane', paneOf(macd).id === 'price');
check('a bad move target is a TypeError', throws(() => rsi.moveTo(5), /move target/, 'TypeError'));
const removingPane = paneOf(rsi).id;
chart.panes.remove(removingPane);
await settle();
check('panes.remove removes the pane and its indicators', !chart.panes.list().some((p) => p.id === removingPane) && !chart.indicators().some((h) => h.id === rsi.id));

// ── 6. view: range, pan, preset, resize ──

const vc0 = count('viewport:changed');
chart.setVisibleRange({ from: data[100].time, to: data[250].time });
await settle();
const r1 = chart.getVisibleRange();
check('setVisibleRange moves the range and fires viewport:changed', r1.from >= data[99].time && r1.to <= data[251].time && count('viewport:changed') > vc0, JSON.stringify(r1));
check('setVisibleRange chains', chart.setVisibleRange({ from: data[100].time, to: data[250].time }) === chart);
const vc1 = count('viewport:changed');
chart.panBy(-0.2);
await settle(8);
check('panBy moves the view into history', chart.getVisibleRange().from < r1.from && count('viewport:changed') > vc1, `${r1.from} -> ${chart.getVisibleRange().from}`);
for (const preset of ['1D', '1W', '1M', '3M', '6M', '1Y', '5Y', 'YTD', 'ALL']) {
    chart.setVisibleRangePreset(preset);
}
await settle();
const all = chart.getVisibleRange();
check('every preset the original lists is accepted; ALL frames every bar', all.from <= data[1].time && all.to >= data.at(-2).time, JSON.stringify(all));
host.rect = { left: 0, top: 0, width: 700, height: 420 };
for (const o of observers) if (o.connected) o.cb([]);
chart.resize();
await settle();
check('resize relayouts the canvases', pile.canvases.every((c) => c.height === 420), pile.canvases.map((c) => c.width + 'x' + c.height).join(' '));

// ── 7. renderer control ──

const rc = chart.renderer;
check('renderer reports its name and capabilities', rc.name === 'native' && rc.capabilities.panes === true && rc.capabilities.paneManagement === true && rc.capabilities.userDrawings === true && 'timelineMarks' in rc.capabilities);
check('supports/get read features', rc.supports('logScale') && rc.supports('glow') && !rc.supports('no-such-feature') && rc.get('logScale') === false && rc.get('no-such-feature') === undefined);
check('set(feature, value) applies and chains', rc.set('logScale', true) === rc && rc.get('logScale') === true);
rc.set({ logScale: false, currentPriceLine: false, upColor: '#00ff88' });
check('set({ ... }) applies several features at once', rc.get('logScale') === false && rc.get('currentPriceLine') === false && rc.get('upColor') === '#00ff88');
const w0 = warnings.length;
rc.set('definitely-not-a-feature', 1);
check('an unsupported feature warns and is ignored', warnings.length === w0 + 1 && /no feature "definitely-not-a-feature"/.test(warnings.at(-1)), warnings.at(-1));
const ps0 = count('priceStyle:change');
rc.set('priceStyle', 'line');
check('priceStyle:change fires before the repaint with from and to', count('priceStyle:change') === ps0 + 1 && last('priceStyle:change').from === 'candles' && last('priceStyle:change').to === 'line');
rc.set('priceStyle', 'candles');
const cfg = rc.getConfig();
check('getConfig returns a versioned document', cfg && typeof cfg === 'object' && isNum(cfg.version ?? 1));
let cfgChanges = 0;
const offCfg = rc.onConfigChanged(() => (cfgChanges += 1));
rc.applyConfig({ ...cfg, layout: { ...(cfg.layout ?? {}) } });
rc.set('logScale', true);
rc.applyConfig(cfg);
check('applyConfig(getConfig()) round-trips the feature values', rc.get('logScale') === false && rc.get('upColor') === '#00ff88', `logScale ${rc.get('logScale')}`);
check('onConfigChanged fires on a config change and its unsubscribe stops it', cfgChanges >= 1 && typeof offCfg === 'function' && (offCfg(), offCfg(), true));
const seenCfg = cfgChanges;
rc.applyConfig(cfg);
check('after unsubscribing no more config changes arrive', cfgChanges === seenCfg);
check('screenshot returns a PNG data URL', /^data:image\/png/.test(rc.screenshot() ?? ''));
rc.set('priceStyle', 'candles');
check('settings visibility and ids', Array.isArray(rc.listSettingsIds()) && rc.listSettingsIds().length > 5 && rc.setSettingsVisibility({ hidden: ['advanced'] }) === rc && throws(() => rc.setSettingsVisibility({ hidden: 'x' }), /array of setting ids/, 'TypeError'));
check('openSettings and closeDialogs chain', rc.openSettings('Canvas') === rc && rc.closeDialogs() === rc);
check('layout mode accepts mobile and desktop only', rc.setLayoutMode('mobile') === rc && rc.setLayoutMode('desktop') === rc && throws(() => rc.setLayoutMode('tablet'), /"mobile" or "desktop"/, 'TypeError'));
check('the external crosshair seam exists', rc.supportsExternalCrosshair === true && rc.setExternalCrosshair(data[120].time, 100) === rc && rc.setExternalCrosshair(null) === rc);
const clicks = [];
const crosshairs = [];
const offClick = rc.onClick((e) => clicks.push(e));
const offCross = rc.onCrosshairMove((e) => crosshairs.push(e));
const px = (type, x, y) => dataCanvas.dispatch(type, { clientX: x, clientY: y, pointerId: 9, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
px('pointermove', 400, 150);
pump();
check('onCrosshairMove reports time, price, pane and the bar under the cursor', crosshairs.length >= 1 && (() => {
    const e = crosshairs.at(-1);
    return isNum(e.time) && isNum(e.price) && e.paneKind === 'price' && e.paneId === 'price' && e.values instanceof Map && e.ohlc && isNum(e.ohlc.open) && isNum(e.ohlc.time);
})(), JSON.stringify(crosshairs.at(-1)));
px('pointerdown', 400, 150);
px('pointerup', 400, 150);
check('onClick reports the clicked bar and pane', clicks.length === 1 && isNum(clicks[0].time) && clicks[0].paneKind === 'price' && clicks[0].paneId === 'price' && isNum(clicks[0].price));
offClick();
offCross();
px('pointerdown', 410, 150);
px('pointerup', 410, 150);
check('unsubscribed listeners stay quiet', clicks.length === 1);
check('dataWindowReadout answers a snapshot or null', (() => {
    const r = rc.dataWindowReadout();
    return r === null || (str(r.date) && Array.isArray(r.groups));
})());
rc.setExternalCrosshair(null);

// ── 8. drawings ──

const dr = chart.drawings;
check('drawings.supported', dr.supported === true);
const created0 = count('drawing:created');
const hline = dr.add('hline', { anchors: [{ time: data[150].time, price: data[150].close }] });
check('drawings.add returns the record and fires drawing:created', hline && str(hline.id) && hline.type === 'hline' && hline.anchors.length === 1 && last('drawing:created')?.id === hline.id && count('drawing:created') === created0 + 1);
const trend = dr.add('trendline', { anchors: [{ time: data[100].time, price: data[100].close }, { time: data[200].time, price: data[200].close }], style: { lineColor: '#ff00aa' } });
check('add takes anchors and a partial style', trend.type === 'trendline' && trend.style.lineColor === '#ff00aa');
check('all() lists drawings in paint order', dr.all().length === 2 && dr.all()[0].id === hline.id);
check('add of an unknown tool type returns null', dr.add('no-such-tool') === null && dr.all().length === 2);
const edited0 = count('drawing:edited');
dr.update(trend.id, { style: { lineWidth: 4 } });
check('update applies a partial record and fires drawing:edited', dr.all().find((d) => d.id === trend.id).style.lineWidth === 4 && dr.all().find((d) => d.id === trend.id).style.lineColor === '#ff00aa' && count('drawing:edited') > edited0 && last('drawing:edited').id === trend.id);
dr.updateMany([{ id: trend.id, patch: { locked: true } }, { id: hline.id, patch: { visible: false } }]);
check('updateMany applies several patches', dr.all().find((d) => d.id === trend.id).locked === true && dr.all().find((d) => d.id === hline.id).visible === false);
dr.show(hline.id);
dr.lock(trend.id, false);
check('show and lock flip single drawings (default true)', dr.all().find((d) => d.id === hline.id).visible === true && dr.all().find((d) => d.id === trend.id).locked === false);
dr.select(trend.id);
check('select fires drawing:selected with the primary id and the list', last('drawing:selected')?.id === trend.id && last('drawing:selected').ids.join() === trend.id);
dr.select([trend.id, hline.id]);
check('select takes an array', last('drawing:selected').ids.length === 2 && last('drawing:selected').id === trend.id);
dr.select(null);
check('select(null) clears the selection', last('drawing:selected').id === null && last('drawing:selected').ids.length === 0);
dr.openSettings(trend.id);
check('openSettings fires drawing:settings', last('drawing:settings')?.id === trend.id);
dr.select(null);
dr.bringToFront(hline.id);
check('bringToFront reorders paint order', dr.all().at(-1).id === hline.id);
dr.sendToBack(hline.id);
check('sendToBack reorders paint order', dr.all()[0].id === hline.id);
check('undo and redo are core-owned history', dr.canUndo() === true && (dr.undo(), dr.canRedo()) === true && (dr.redo(), dr.canUndo()) === true);
const doc1 = dr.toJSON();
check('toJSON is a versioned document', doc1.version === 1 && doc1.drawings.length === 2 && JSON.stringify(dr.getConfig()) === JSON.stringify(doc1));
dr.clone(trend.id);
check('clone duplicates in place and selects the copy', dr.all().length === 3);
dr.duplicate([hline.id]);
check('duplicate copies several', dr.all().length === 4);
dr.copyToClipboard([trend.id]);
dr.paste();
check('copyToClipboard and paste make a fresh drawing', dr.all().length === 5 && new Set(dr.all().map((d) => d.id)).size === 5);
const removed0 = count('drawing:removed');
dr.removeMany(dr.all().slice(2).map((d) => d.id));
check('removeMany removes several as one step, firing drawing:removed per drawing', dr.all().length === 2 && count('drawing:removed') === removed0 + 3);
dr.undo();
check('one undo brings the removed ones back', dr.all().length === 5);
dr.fromJSON(doc1);
check('fromJSON restores a document, keeping ids', dr.all().length === 2 && dr.all().map((d) => d.id).sort().join() === [hline.id, trend.id].sort().join());
dr.fromJSON({ nonsense: true });
check('a missing or invalid document restores an empty chart', dr.all().length === 0 && dr.toJSON().drawings.length === 0);
dr.applyConfig(doc1);
check('applyConfig is fromJSON', dr.all().length === 2);
dr.remove(hline.id);
check('remove fires drawing:removed', last('drawing:removed')?.id === hline.id && dr.all().length === 1);
check('drawings chain', dr.lock(trend.id) === dr && dr.show(trend.id) === dr && dr.undo() === dr);
dr.fromJSON(doc1);
dr.setTool('trendline');
check('setTool arms and fires drawing:tool', dr.getTool() === 'trendline' && last('drawing:tool')?.type === 'trendline');
dr.setTool(null);
check('setTool(null) returns to select', dr.getTool() === null && last('drawing:tool').type === null);
dr.setSnapMode('strong');
check('setSnapMode fires drawing:snap', dr.getSnapMode() === 'strong' && last('drawing:snap').mode === 'strong');
dr.setStayMode(true);
check('setStayMode fires drawing:stay', dr.getStayMode() === true && last('drawing:stay').on === true);
dr.setMode('measure');
check('setMode fires drawing:mode', dr.getMode() === 'measure' && last('drawing:mode').mode === 'measure');
dr.setMode(null);
check('setMode(null) leaves the mode', dr.getMode() === null && last('drawing:mode').mode === null);
dr.setFavorite('trendline', true);
check('favorites follow setFavorite and fire drawing:favorites', dr.isFavorite('trendline') && dr.favorites().includes('trendline') && last('drawing:favorites').favorites.includes('trendline'));
dr.setFavorites(['box', 'hline']);
check('setFavorites replaces the set', dr.favorites().join() === 'box,hline');
dr.setToolDefaults({ trendline: { style: { lineColor: '#123456' } } });
check('setToolDefaults fires drawing:defaults and toolDefaults reads them back', last('drawing:defaults')?.type === 'trendline' && dr.toolDefaults().trendline?.style?.lineColor === '#123456');
dr.resetToolDefaults('trendline');
check('resetToolDefaults forgets one tool', dr.toolDefaults().trendline === undefined);
dr.setToolTemplates({ trendline: [{ name: 'Bold', settings: { style: { lineWidth: 5 } } }] });
check('setToolTemplates fires drawing:templates and toolTemplates reads them back', last('drawing:templates')?.type === 'trendline' && dr.toolTemplates().trendline?.some((t) => t.name === 'Bold' && t.settings.style.lineWidth === 5));
check('toolbar calls chain', dr.showToolbar(false) === dr && dr.showToolbar() === dr && dr.setToolbar({ tools: ['trendline', 'hline', 'box'] }) === dr && dr.setToolbar(true) === dr && dr.setToolShortcuts({ trendline: 'Alt+T' }) === dr && dr.setExternalGhost(null) === dr);
check('setToolbar takes the original option shapes', throws(() => dr.setToolbar(5), /boolean or an object/, 'TypeError') && throws(() => dr.setToolbar({ tools: 'x' }), /array of tool type keys/, 'TypeError'));
// Interactive placement: the draft event while placing.
dr.fromJSON({ version: 1, drawings: [] });
// the toolbar gutter: the data canvas is narrower than the host by the docked bar
const off = host.rect.width - dataCanvas.width;
const mouse = (type, x, y) => dataCanvas.dispatch(type, { clientX: x + off, clientY: y, pointerId: 5, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, timeStamp: clock });
const drafts0 = count('drawing:draft');
dr.setTool('trendline');
mouse('pointerdown', 300, 200);
mouse('pointerup', 300, 200);
mouse('pointermove', 450, 150);
pump();
check('placing by clicks fires drawing:draft with the ghost, then null when placed', count('drawing:draft') > drafts0 && fired.get('drawing:draft').some((p) => p.doc && p.doc.type === 'trendline'));
mouse('pointerdown', 450, 150);
mouse('pointerup', 450, 150);
await settle();
check('two clicks place the drawing and the draft ends', dr.all().length === 1 && last('drawing:draft').doc === null, `${dr.all().length} drawings`);
dr.fromJSON({ version: 1, drawings: [] });

// ── 9. timeline marks ──

const marks = chart.marks;
check('marks.supported', marks.supported === true);
let buttonRuns = 0;
marks
    .defineGroup({ id: 'dividends', label: 'Dividends' })
    .defineGroup({ id: 'splits', label: 'Splits', visible: false });
check('marks chain and groups() lists the definitions in order', marks.groups().map((g) => g.id).join() === 'dividends,splits' && marks.groups()[1].visible === false);
check('group visibility: the declared default, then the user switch', marks.isGroupVisible('dividends') === true && marks.isGroupVisible('splits') === false && (marks.setGroupVisible('splits'), marks.isGroupVisible('splits')) === true && (marks.setGroupVisible('splits', false), marks.isGroupVisible('splits')) === false);
marks.add({
    id: 'div-1',
    time: data[200].time,
    title: 'Dividends',
    group: 'dividends',
    glyph: { shape: 'circle', color: '#2962ff', letter: 'D' },
    tooltip: 'Dividend',
    content: { panel: { items: [{ type: 'field', label: 'Amount', value: '0.01' }, { type: 'text', text: 'note' }, { type: 'button', label: 'More', primary: true, run: () => (buttonRuns += 1) }] } },
});
check('marks.add stores the mark and chains', marks.all().length === 1 && marks.all()[0].id === 'div-1' && marks.all()[0].glyph.letter === 'D' && marks.all()[0].content.panel.items.length === 3);
const panelButton = marks.all()[0].content.panel.items[2];
panelButton.run();
check('a panel button keeps its run() function', typeof panelButton.run === 'function' && buttonRuns === 1);
marks.add({ id: 'div-1', time: data[200].time, glyph: { color: '#fff', icon: 'x' }, content: { html: '<b>x</b>' } });
check('an existing id is replaced in place', marks.all().length === 1 && marks.all()[0].glyph.icon === 'x' && marks.all()[0].content.html === '<b>x</b>');
marks.set([
    { id: 'm-a', time: data[210].time, glyph: { color: '#26a69a', letter: 'A' }, group: 'dividends', content: { text: 'Plain text' } },
    { id: 'm-b', time: data[240].time, glyph: { color: '#ef5350', shape: 'diamond', letter: 'B' } },
]);
check('marks.set replaces the whole set', marks.all().map((m) => m.id).join() === 'm-a,m-b');
marks.remove('m-b');
check('marks.remove drops one mark', marks.all().map((m) => m.id).join() === 'm-a');
dr.setTool(null);
dr.setStayMode(false);
chart.setVisibleRange({ from: data[150].time, to: data.at(-1).time });
await settle();
// A click on the glyph: scan the lane above the time axis for it.
const markClicks0 = count('mark:click');
const plotW = dataCanvas.width;
const plotH = dataCanvas.height;
let found = null;
for (let y = plotH - 8; y > plotH - 90 && !found; y -= 4) {
    for (let x = 20; x < plotW - 20 && !found; x += 4) {
        mouse('pointerdown', x, y);
        mouse('pointerup', x, y);
        if (count('mark:click') > markClicks0) found = [x, y];
    }
}
check('clicking the glyph fires mark:click with the id, the ids and the time', found && last('mark:click').id === 'm-a' && last('mark:click').ids.join() === 'm-a' && last('mark:click').time === data[210].time && last('mark:click').group === 'dividends', JSON.stringify(last('mark:click')));
marks.clear();
check('marks.clear drops them all', marks.all().length === 0);

// ── 10. replay ──

const rp = chart.replay;
check('replay starts inactive', rp.state.active === false && rp.state.playing === false && rp.state.cursorTime === null);
chart.setVisibleRangePreset('ALL');
await rp.start({ from: data[200].time });
await settle();
check('replay.start hides the later bars and fires replay:start', rp.state.active === true && rp.state.cursorTime === data[200].time && rp.state.remaining === 99 && last('replay:start')?.cursorTime === data[200].time && last('replay:start').remaining === 99, JSON.stringify(rp.state));
check('a replay starts paused, with nextTime and an interval', rp.state.playing === false && rp.state.nextTime === data[201].time && isNum(rp.state.intervalMs));
const bar0 = count('bar');
check('step reveals the next bar and fires replay:step and bar', rp.step() === true && rp.state.cursorTime === data[201].time && last('replay:step')?.cursorTime === data[201].time && last('replay:step').remaining === 98 && count('bar') > bar0 && last('bar').time === data[201].time);
check('stepUpdate without tick replay reveals a whole bar', rp.stepUpdate() === true && rp.state.cursorTime === data[202].time);
check('replay calls chain', rp.pause() === rp && rp.stop !== undefined);
rp.play(15);
check('play fires replay:play with the interval and marks the state', last('replay:play')?.intervalMs === 15 && rp.state.playing === true && rp.state.intervalMs === 15);
await until(() => rp.state.cursorTime >= data[206].time, 'timed reveal');
check('a timed replay reveals bars on its own', rp.state.cursorTime >= data[206].time);
rp.pause();
check('pause fires replay:pause', count('replay:pause') >= 1 && rp.state.playing === false);
const ticksSeen = [];
rp.setTicks(async (bar, { end, signal }) => {
    ticksSeen.push({ time: bar.time, end, hasSignal: signal !== undefined && typeof signal.aborted === 'boolean' });
    return [{ price: bar.open }, { price: (bar.open + bar.close) / 2, high: bar.high, low: bar.low }, { price: bar.close }];
});
rp.play(8);
await until(() => count('replay:tick') >= 3, 'replay ticks');
rp.pause();
check('setTicks plays a bar as intrabar updates and fires replay:tick', count('replay:tick') >= 3 && ticksSeen.length >= 1 && ticksSeen[0].end === ticksSeen[0].time + HOUR && ticksSeen[0].hasSignal, JSON.stringify(ticksSeen[0]));
rp.setTicks(null);
await rp.start({ from: data[100].time });
check('start while replaying seeks (and pauses)', rp.state.cursorTime === data[100].time && rp.state.playing === false);
rp.stop();
await settle();
check('stop restores the history and fires replay:end with stopped', rp.state.active === false && last('replay:end')?.reason === 'stopped' && rp.bounds.last === data.at(-1).time);
await rp.start({ from: data.at(-3).time });
rp.step();
rp.step();
check('revealing the last bar ends the replay with finished', last('replay:end')?.reason === 'finished' && rp.state.active === false, JSON.stringify(last('replay:end')));
const inactiveStep = rp.step();
check('step while off returns false', inactiveStep === false);
const tickSource = wasm.lowerTimeframeTicks(chart, '1');
check('lowerTimeframeTicks returns a tick source function; barsToTicks maps bars', typeof tickSource === 'function' && wasm.barsToTicks(data.slice(0, 3)).length === 3 && throws(() => wasm.barsToTicks(5), /array of bars/, 'TypeError'));

// ── 11. theme and locale ──

const theme0 = count('theme:changed');
check('setTheme chains and fires theme:changed with the resolved theme', chart.setTheme('light') === chart && count('theme:changed') === theme0 + 1 && last('theme:changed').background !== '#151619');
chart.setTheme({ background: '#101010', textColor: '#eeeeee', gridColor: '#222222', borderColor: '#333333', upColor: '#00cc66', downColor: '#cc0044', fontFamily: 'sans-serif' });
check('setTheme takes a theme object', last('theme:changed').background === '#101010' && last('theme:changed').upColor === '#00cc66');
chart.setTheme('no-such-theme');
check('an unknown theme id falls back to dark, as in the original', last('theme:changed').background === '#151619', last('theme:changed').background);
chart.setTheme('dark');
check('i18n starts English, ltr, latn, h23', chart.i18n.locale === 'en' && chart.i18n.direction === 'ltr' && chart.i18n.numberingSystem === 'latn' && chart.i18n.hourCycle === 'h23');
const loc0 = count('locale:changed');
check('setLocale chains', chart.setLocale('tr') === chart);
check("setLocale('tr') emits locale:changed and the same i18n object reads the new locale", count('locale:changed') === loc0 + 1 && last('locale:changed').locale === 'tr' && last('locale:changed').direction === 'ltr' && chart.i18n.locale === 'tr');
chart.setLocale({ locale: 'ar', direction: 'auto' });
check('direction auto follows the locale script and the root element carries lang and dir', chart.i18n.direction === 'rtl' && last('locale:changed').direction === 'rtl' && host.children[0].getAttribute('dir') === 'rtl' && host.children[0].getAttribute('lang') === 'ar');
chart.setLocale({ locale: undefined, direction: undefined });
check('an explicit undefined restores the default', chart.i18n.locale === 'en' && chart.i18n.direction === 'ltr');
chart.setLocale({ hourCycle: 'h12', weekStart: 1, numberingSystem: 'arab' });
check('hourCycle, weekStart and numberingSystem change at runtime', chart.i18n.hourCycle === 'h12' && chart.i18n.weekStart === 1 && chart.i18n.numberingSystem === 'arab');
chart.setLocale({ hourCycle: undefined, weekStart: undefined, numberingSystem: undefined });
check('i18n.t returns a plain string unchanged and a descriptor in English', chart.i18n.t('plain') === 'plain' && chart.i18n.t({ id: 'x.y', defaultMessage: 'Hello {name}' }, { name: 'Vela' }) === 'Hello Vela');
check('i18n.format formats with the chart locale', chart.i18n.format.price(1234.5, { decimals: 2 }) === '1234.50' && chart.i18n.format.price(1234.5, { decimals: 2, grouping: true }) === '1,234.50' && chart.i18n.format.compact(1_200_000) === '1.2M' && chart.i18n.format.percent(1.5) === '+1.50%');
const offLocale = wasm.registerLocale({ locale: 'xx', messages: { 'a.b': 'Zed {n}' } });
chart.setLocale('xx');
check('a registerLocale pack translates descriptors through chart.i18n', chart.i18n.locale === 'xx' && chart.i18n.t({ id: 'a.b', defaultMessage: 'Eng {n}' }, { n: 3 }) === 'Zed 3');
offLocale();
chart.setLocale('en');

// ── 12. data: a provider written in JavaScript ──

const providerCalls = [];
const jsProvider = {
    async getBars(ticker, timeframe, range) {
        providerCalls.push({ ticker, timeframe, range });
        await macrotask(1);
        if (ticker === 'BAD') throw new Error('provider exploded');
        if (ticker === 'REJECT') return Promise.reject(new wasm.DataUnavailableError({ reason: 'timeframe', symbol: 'REJECT', timeframe, finest: '1D', default: '1D' }));
        if (ticker === 'PLAIN') throw Object.assign(new Error('GDP is not served at this timeframe'), { name: 'DataUnavailableError', reason: 'timeframe', finest: '1D', default: '1D' });
        if (ticker === 'DEEP') return gridBars(HOUR, 20000, range);
        return gridBars(HOUR, 600, range);
    },
    async listSymbols() {
        return [{ ticker: 'AAA', description: 'Alpha Corp', type: 'stock', prefix: 'NASDAQ' }, { ticker: 'BBB', description: 'Beta' }, { ticker: 'DEEP' }];
    },
    async getSymbolInfo(ticker) {
        if (ticker === 'BBB') return { ticker, ohlc: false, volume: false, timezone: 'Etc/UTC' };
        return { ticker, timezone: 'America/New_York', session: '0930-1600' };
    },
    info() {
        return { name: 'js', displayName: 'JS provider', capabilities: { enumerate: true, stream: false, symbolInfo: true } };
    },
};
const unresolved0 = count('data:unresolved');
let parkedSettled = false;
const parked = chart.setMarket({ symbol: 'AAA', timeframe: '60' }).then(() => {
    parkedSettled = true;
});
await settle();
check('with no provider at all the load parks quietly', parkedSettled === false && count('data:unresolved') === unresolved0 && chart.data.providers().length === 0);
// data:unresolved: a provider whose index settles without the symbol.
chart.data.registerProvider('decoy', { getBars: async () => [], listSymbols: async () => [{ ticker: 'ZZZ' }] });
await chart.data.ready();
await settle();
check('a parked symbol no index lists fires data:unresolved with the providers tried', count('data:unresolved') === unresolved0 + 1 && last('data:unresolved').symbol === 'AAA' && last('data:unresolved').providers.join() === 'decoy' && parkedSettled === false, JSON.stringify(last('data:unresolved')));
chart.data.unregisterProvider('decoy');
check('the registry starts empty', chart.data.providers().length === 0 && chart.data.resolve('AAA') === null && chart.data.symbols().length === 0 && chart.data.capabilities('AAA') === null);
check('registerProvider chains and returns the provider back through providerInstance', chart.data.registerProvider('js', jsProvider) === chart.data && chart.data.providerInstance('JS') === jsProvider);
await chart.data.ready();
check('providers() lists the metadata the provider states', (() => {
    const list = chart.data.providers();
    return list.length === 1 && list[0].name === 'js' && list[0].displayName === 'JS provider' && list[0].capabilities.enumerate === true;
})(), JSON.stringify(chart.data.providers()));
check('symbols() lists the index with descriptors', (() => {
    const s = chart.data.symbols('js');
    return s.length === 3 && s.find((x) => x.ticker === 'AAA').description === 'Alpha Corp' && s.find((x) => x.ticker === 'AAA').prefix === 'NASDAQ' && chart.data.symbols().length === 3;
})());
check('resolve, displayPrefix and canonicalSymbol route through the provider', JSON.stringify(chart.data.resolve('AAA')) === '{"provider":"js","ticker":"AAA"}' && chart.data.resolve('js:BBB').ticker === 'BBB' && chart.data.resolve('NASDAQ:AAA')?.ticker === 'AAA' && chart.data.displayPrefix('AAA') === 'NASDAQ' && chart.data.displayPrefix('BBB') === 'js' && chart.data.canonicalSymbol('AAA') === 'NASDAQ:AAA' && chart.data.canonicalSymbol('BBB') === 'js:BBB');
check('capabilities(symbol) reads the resolved capabilities', chart.data.capabilities('AAA')?.symbolInfo === true);
const info = await chart.data.symbolInfo('AAA');
check('symbolInfo resolves the provider facts, and undefined for an unknown symbol', info?.ticker === 'AAA' && info.timezone === 'America/New_York' && (await chart.data.symbolInfo('nope:NOPE')) === undefined);
await parked;
await settle();
check('registering the provider that resolves the symbol fires the parked load', parkedSettled === true && chart.replay.bounds?.last === T0 + 599 * HOUR && last('market:changed').symbol === 'AAA', JSON.stringify(chart.replay.bounds));
// symbol and interval switches through the JS provider
const mc0 = count('market:changed');
const ls0 = count('load:start');
const le0 = count('load:end');
await chart.setMarket({ symbol: 'js:AAA', timeframe: '60' });
await settle();
check('setMarket switches the symbol and fires market:changed with the previous identity', count('market:changed') === mc0 + 1 && last('market:changed').symbol === 'js:AAA' && last('market:changed').timeframe === '60' && last('market:changed').prev.symbol === 'AAA');
check('load:start and load:end bracket the switch with the bar count', count('load:start') === ls0 + 1 && last('load:start').firstLoad === false && last('load:start').symbol === 'js:AAA' && count('load:end') === le0 + 1 && last('load:end').bars === 500, JSON.stringify(last('load:end')));
check('the provider got the ticker without its prefix, the timeframe and a range', providerCalls.some((c) => c.ticker === 'AAA' && c.timeframe === '60'));
check('market reads the requested market', JSON.stringify(chart.market) === JSON.stringify({ symbol: 'js:AAA', provider: 'js', timeframe: '60', offline: false }), JSON.stringify(chart.market));
const bounds2 = chart.replay.bounds;
check('the bars from the promise are on the chart (the default depth of 500 asked of the provider)', bounds2.first === T0 + 100 * HOUR && bounds2.last === T0 + 599 * HOUR && providerCalls.some((c) => c.range.limit === 500), JSON.stringify(bounds2));
check('the new bars are painted', painted(dataCanvas));
const calls0 = providerCalls.length;
await chart.setMarket({ timeframe: '240' });
await settle();
check('an interval switch reloads through the provider and keeps the symbol', last('market:changed').timeframe === '240' && last('market:changed').symbol === 'js:AAA' && last('market:changed').prev.timeframe === '60' && providerCalls.length > calls0 && providerCalls.at(-1).timeframe === '240');
check('indicators and panes survive a market switch', chart.indicators().some((h) => h.nativeType === 'moving-average'));
const facts0 = count('symbol:facts');
await chart.setMarket({ symbol: 'js:BBB', timeframe: '60' });
await until(() => chart.symbolFacts()?.ohlc === false, 'symbol facts');
await settle();
check('symbol facts land with symbol:facts and symbolFacts()', count('symbol:facts') > facts0 && chart.symbolFacts()?.ohlc === false && chart.symbolFacts()?.volume === false && fired.get('symbol:facts').some((p) => p.facts?.ohlc === false), JSON.stringify(chart.symbolFacts()));
const noVolHost = container(600, 400);
const noVol = new Vela(noVolHost, { symbol: 'js:BBB', timeframe: '60' });
noVol.data.registerProvider('js', jsProvider);
await noVol.ready();
await settle();
check('a symbol whose source states no volume gets no automatic volume indicator', noVol.symbolFacts()?.volume === false && !noVol.presentNativeIndicators().includes('volume') && noVol.pendingNativeIndicators().length === 0, JSON.stringify(noVol.presentNativeIndicators()));
noVol.destroy();
// a rejected promise
const le1 = count('load:end');
await chart.setMarket({ symbol: 'js:BAD', timeframe: '60' });
await settle();
check('a rejected promise reads as an empty answer: load:end with 0 bars, no throw', count('load:end') > le1 && last('load:end').bars === 0 && last('load:end').symbol === 'js:BAD', JSON.stringify(last('load:end')));
const un0 = count('data:unavailable');
await chart.setMarket({ symbol: 'js:REJECT', timeframe: '60' });
await settle();
check('a rejection with a DataUnavailableError fires data:unavailable with the hints', count('data:unavailable') === un0 + 1 && last('data:unavailable').symbol === 'js:REJECT' && last('data:unavailable').finest === '1D' && last('data:unavailable').default === '1D' && last('data:unavailable').reason === 'timeframe', JSON.stringify(last('data:unavailable')));
await chart.setMarket({ symbol: 'js:PLAIN', timeframe: '60' });
await settle();
check('any object named DataUnavailableError with a reason counts, as in the original', count('data:unavailable') === un0 + 2 && last('data:unavailable').symbol === 'js:PLAIN');
check('isDataUnavailableError reads the name and the fields, never the class', wasm.isDataUnavailableError(new wasm.DataUnavailableError({ reason: 'timeframe' })) && wasm.isDataUnavailableError({ name: 'DataUnavailableError', reason: 'x', message: 'm' }) && !wasm.isDataUnavailableError(new Error('x')) && !wasm.isDataUnavailableError(null) && !wasm.isDataUnavailableError({ name: 'DataUnavailableError', reason: 'x', message: 'm', finest: 5 }));
const refusedErr = new wasm.DataUnavailableError({ reason: 'timeframe', symbol: 'GDP', timeframe: '60', finest: '1D' });
check('DataUnavailableError builds the original message and exposes its fields', refusedErr.name === 'DataUnavailableError' && refusedErr.message === 'GDP is not served at 60; its finest timeframe is 1D' && refusedErr.reason === 'timeframe' && refusedErr.finest === '1D' && refusedErr.default === undefined && String(refusedErr) === `DataUnavailableError: ${refusedErr.message}`);
check('unregisterProvider removes the provider', chart.data.unregisterProvider('js') === chart.data && chart.data.providers().length === 0 && chart.data.providerInstance('js') === undefined);
check('the bundled exchange provider classes exist, are constructed and not registered', new wasm.BinanceProvider().__velaNativeProvider === 'binance' && new wasm.CoinbaseProvider().__velaNativeProvider === 'coinbase' && new wasm.HyperliquidProvider().__velaNativeProvider === 'hyperliquid');

// A deep history through the JS provider: the backfill reports history:progress.
const deepHost = container(800, 500);
const deep = new Vela(deepHost, { symbol: 'js:DEEP', timeframe: '60', bars: 12000 });
watchAll(deep, 'deep');
deep.data.registerProvider('js', jsProvider);
await deep.ready();
await deep.historyComplete();
await settle();
check('a deep history paints a head first and backfills, firing history:progress', count('history:progress') >= 1 && last('history:progress').target >= last('history:progress').loaded, `${count('history:progress')} progress events`);
check('history:complete reports the depth with the oldest time and the count', last('history:complete').barsLoaded >= 10000 || last('history:complete').reason === 'genesis', JSON.stringify(last('history:complete')));
deep.destroy();

// ── 13. data: a market-data feed written in JavaScript, and the memory provider ──

let feedSink = null;
const feedLoads = [];
const jsFeed = {
    async load(cfg) {
        feedLoads.push(cfg);
        return gridBars(HOUR, 200);
    },
    subscribe(cfg, onBar) {
        feedSink = onBar;
        return () => {
            feedSink = null;
        };
    },
    symbolInfo() {
        return { ticker: 'FEED', volume: true };
    },
    destroy() {
        feedLoads.push('destroyed');
    },
};
const feedHost = container(800, 500);
const feedChart = new Vela(feedHost, { symbol: 'FEED', timeframe: '60', live: true }, { dataFeed: jsFeed });
watchAll(feedChart, 'feed');
await feedChart.ready();
await settle();
check('a JS feed serves the history through promises and receives the market config', feedLoads.length >= 1 && feedLoads[0].symbol === 'FEED' && feedLoads[0].timeframe === '60' && feedChart.replay.bounds?.last === T0 + 199 * HOUR);
check('the feed is subscribed for live ticks (live: true)', typeof feedSink === 'function');
const lastBar = gridBars(HOUR, 200).at(-1);
const bars0 = count('bar');
feedSink({ ...lastBar, close: lastBar.close + 2, high: lastBar.high + 3 });
feedSink({ time: lastBar.time + HOUR, open: 100, high: 102, low: 99, close: 101, volume: 7 });
await settle();
check('live ticks from the feed fire the bar event with the original shape', count('bar') === bars0 + 2 && last('bar').time === lastBar.time + HOUR && last('bar').volume === 7, JSON.stringify(last('bar')));
check('a new live bar appends', feedChart.replay.bounds.last === lastBar.time + HOUR);
check('with a custom feed chart.data warns and does nothing, as in the original', (() => {
    const before = warnings.length;
    feedChart.data.registerProvider('x', { getBars: async () => [] });
    return warnings.length === before + 1 && /custom feed/.test(warnings.at(-1)) && feedChart.data.providers().length === 0;
})());
check('a feed without load and subscribe is refused', throws(() => new Vela(container(100, 100), { data }, { dataFeed: { load() {} } }), /deps\.dataFeed must implement load/, 'TypeError'));
feedChart.destroy();
check('destroy asks the feed to release its resources and unsubscribes it', feedLoads.includes('destroyed') && feedSink === null);

const memHost = container(800, 500);
const memory = new Vela(memHost, { bars: { MEM: { 60: gridBars(HOUR, 150) } }, symbols: { MEM: { description: 'In memory', prefix: 'LOCAL' } } });
await memory.ready();
await settle();
check('bars as an object seeds the in-memory provider and picks its first symbol', memory.market.symbol === 'MEM' && memory.replay.bounds?.last === T0 + 149 * HOUR && memory.data.providers().some((p) => p.name === 'memory') && memory.data.symbols('memory')[0].description === 'In memory');
memory.destroy();
check('bars as a number is the history depth', (() => {
    const c = new Vela(container(100, 100), { bars: 500, data });
    const ok = c.market.bars === 500;
    c.destroy();
    return ok;
})());

// ── 14. every event, and the four that need an engine ──

const missing = ALL_EVENTS.filter((n) => !fired.has(n) && !NEVER_FIRE.includes(n));
check('every chart event that can fire here fired', missing.length === 0, missing.join(','));
check('every payload matches the original shape', shapeFailures.length === 0, shapeFailures.slice(0, 3).join(' | '));
check('the engine events subscribe without error and stay silent', NEVER_FIRE.every((n) => typeof chart.on(n, () => {}) === 'function' && !fired.has(n)));

// ── 15. teardown: no DOM, listeners, timers or frames; calls after destroy ──

const handlerCalls = count('bar');
chart.destroy();
check('destroy removes the DOM', host.children.length === 0 && host.style.getPropertyValue('--vela-scale-gutter') === '');
check('destroy removes every listener it put on the page', totalListeners(host) === listenersBefore && doc.listeners.length === docListenersBefore && doc.defaultView.listeners.length === 0, `${totalListeners(host)} vs ${listenersBefore}`);
check('destroy disconnects every observer and media query it armed', observers.slice(observersBefore).length >= 1 && observers.slice(observersBefore).every((o) => !o.connected) && dprWatchers.every((q) => q.listeners.length === 0));
await settle();
check('destroy leaves no frame pending', frames.size === 0, `${frames.size} frames`);
check('destroy is idempotent', (() => {
    chart.destroy();
    return true;
})());
check('calls after destroy are quiet no-ops and queries answer empty', (() => {
    chart.setTheme('light');
    chart.setLocale('de');
    chart.panBy(0.1);
    chart.resize();
    chart.renderer.set('logScale', true);
    chart.drawings.setTool('trendline');
    chart.panes.collapse('price');
    chart.marks.clear();
    chart.replay.stop();
    return chart.indicators().length === 0 && chart.getVisibleRange() === null && chart.drawings.all().length === 0 && chart.panes.list().length === 0 && chart.marks.all().length === 0 && chart.data.providers().length === 0 && chart.replay.state.active === false && chart.drawings.supported === false;
})());
check('promises after destroy resolve at once', (await chart.ready()) === undefined && (await chart.historyComplete()) === undefined && (await chart.setMarket({ symbol: 'x' })) === undefined && (await chart.availableNativeIndicators()).length === 0);
check('on() after destroy returns an unsubscribe that never fires', (() => {
    let n = 0;
    const off = chart.on('bar', () => (n += 1));
    off();
    return typeof off === 'function' && n === 0;
})());
check('a handle outlives destroy as a quiet object', sma.setVisible(false) === sma && (sma.remove(), true) && (await sma.context()) === null);
check('addIndicator after destroy is an error', throws(() => chart.addIndicator('//x'), /the chart was destroyed/));
check('no event reached a handler after destroy', count('bar') === handlerCalls);
const live = [host, selectorHost, deepHost, feedHost, memHost];
await settle();
check('every chart of this script left no frame or timer behind', frames.size === 0 && live.every((h) => h.children.length === 0) && timers() <= timersBefore + 1, `${timers()} timers (before ${timersBefore})`);
void offs;
void realWarn;

console.warn = realWarn;
if (state.failures > 0) {
    console.error(`\n${state.failures} check(s) failed`);
    process.exit(1);
}
console.log('\nall checks passed');
