// Vela (Rust) workspace demo: a 2x2 grid of charts on synthetic data, no network.
//
// The four series are generated here with fixed seeds, so every load shows the same
// history, and an in-memory provider serves them (the exchange providers are not used).
// A plain row of HTML buttons above the widget switches the layout, the symbol and
// crosshair links and the theme; the buttons are not part of the widget.
//
// localStorage:
//   vela-demo-workspace         the workspace document (layout, splitter sizes, sync links,
//                               each cell's symbol, indicators and drawings)
//   vela-demo-workspace-theme   'dark' or 'light'

import init, { VelaWorkspace } from '../pkg/vela_web.js';

const HOUR = 3_600_000;
const BAR_COUNT = 400;
const START = Date.UTC(2025, 0, 1);
const TICK_MS = 500;
const TICKS_PER_BAR = 20;
const TIMEFRAME = '60';
const STATE_KEY = 'vela-demo-workspace';
const THEME_KEY = 'vela-demo-workspace-theme';

// Four invented instruments: the names mean nothing, the series are random walks.
const SYMBOLS = [
    { name: 'ALFA', level: 100, seed: 20250101 },
    { name: 'BETA', level: 60, seed: 77 },
    { name: 'GAMA', level: 230, seed: 4242 },
    { name: 'DELTA', level: 15, seed: 9001 },
];

// layout id -> label
const LAYOUTS = [
    ['1', 'Tek'],
    ['2h', 'İki yatay'],
    ['2v', 'İki dikey'],
    ['4', 'Dört'],
];

function lcg(seed) {
    let state = seed >>> 0;
    return () => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
        return state / 4294967296;
    };
}

function makeBars({ level, seed }) {
    const rnd = lcg(seed);
    const bars = [];
    let price = level;
    for (let i = 0; i < BAR_COUNT; i += 1) {
        const open = price;
        const drift = Math.sin(i / 37) * 0.0025 * level;
        const close = Math.max(level * 0.2, open + drift + (rnd() - 0.5) * 0.024 * level);
        const high = Math.max(open, close) + rnd() * 0.009 * level;
        const low = Math.min(open, close) - rnd() * 0.009 * level;
        bars.push({ time: START + i * HOUR, open, high, low, close, volume: Math.round(800 + rnd() * 1200) });
        price = close;
    }
    return bars;
}

// localStorage can be missing or blocked: never fatal.
function read(key) {
    try {
        return localStorage.getItem(key);
    } catch {
        return null;
    }
}

function write(key, value) {
    try {
        localStorage.setItem(key, value);
    } catch {
        // Not persisted; the page goes on.
    }
}

function show(message) {
    const el = document.getElementById('message');
    el.textContent = message;
    el.hidden = false;
}

async function main() {
    await init();
    const bars = Object.fromEntries(SYMBOLS.map((s) => [s.name, makeBars(s)]));
    let theme = read(THEME_KEY) === 'light' ? 'light' : 'dark';

    const ws = new VelaWorkspace(document.getElementById('workspace'), {
        layout: '4',
        bars,
        cells: Object.fromEntries(SYMBOLS.map((s) => [s.name.toLowerCase(), { symbol: s.name, timeframe: TIMEFRAME }])),
        theme,
        persist: STATE_KEY,
    });
    window.velaWorkspace = ws;

    // First visit: two indicators on the first chart, so the legend has something to show.
    await ws.ready();
    if (read(STATE_KEY) === null) {
        const first = ws.cell(SYMBOLS[0].name.toLowerCase());
        first.addIndicator('ema', undefined);
        first.addIndicator('rsi', undefined);
    }

    // ── the demo controls ──
    const layoutBox = document.getElementById('layouts');
    const layoutButtons = new Map();
    const known = new Set(ws.layouts().map((l) => l.id));
    for (const [id, label] of LAYOUTS) {
        if (!known.has(id)) continue;
        const button = document.createElement('button');
        button.type = 'button';
        button.textContent = label;
        button.setAttribute('aria-pressed', 'false');
        button.addEventListener('click', () => ws.setLayout(id));
        layoutBox.appendChild(button);
        layoutButtons.set(id, button);
    }
    const syncSymbol = document.getElementById('sync-symbol');
    const syncCrosshair = document.getElementById('sync-crosshair');
    const themeButton = document.getElementById('theme');

    const refresh = () => {
        const layout = ws.layout();
        for (const [id, button] of layoutButtons) button.setAttribute('aria-pressed', String(id === layout));
        syncSymbol.setAttribute('aria-pressed', String(ws.sync('symbol')));
        syncCrosshair.setAttribute('aria-pressed', String(ws.sync('crosshair')));
        themeButton.setAttribute('aria-pressed', String(theme === 'light'));
        themeButton.textContent = theme === 'light' ? 'Koyu tema' : 'Açık tema';
        document.body.style.background = theme === 'dark' ? '#151619' : '#ffffff';
        document.body.style.color = theme === 'dark' ? '#b2b5be' : '#1e293b';
    };
    syncSymbol.addEventListener('click', () => ws.sync('symbol', !ws.sync('symbol')));
    syncCrosshair.addEventListener('click', () => ws.sync('crosshair', !ws.sync('crosshair')));
    themeButton.addEventListener('click', () => {
        theme = theme === 'dark' ? 'light' : 'dark';
        ws.setTheme(theme);
        write(THEME_KEY, theme);
        refresh();
    });
    ws.on('layout:changed', refresh);
    ws.on('sync:changed', refresh);
    refresh();

    // A live feed stand-in: each tick moves the last bar's close; every TICKS_PER_BAR ticks the
    // next hour opens. One walk per symbol.
    const feeds = SYMBOLS.map((s, i) => ({ name: s.name, rnd: lcg(7 + i), last: { ...bars[s.name].at(-1) }, level: s.level }));
    let ticks = 0;
    setInterval(() => {
        ticks += 1;
        for (const feed of feeds) {
            if (ticks % TICKS_PER_BAR === 0) {
                feed.last = { time: feed.last.time + HOUR, open: feed.last.close, high: feed.last.close, low: feed.last.close, close: feed.last.close, volume: 0 };
            }
            const close = Math.max(feed.level * 0.2, feed.last.close + (feed.rnd() - 0.5) * 0.008 * feed.level);
            feed.last = {
                ...feed.last,
                close,
                high: Math.max(feed.last.high, close),
                low: Math.min(feed.last.low, close),
                volume: feed.last.volume + Math.round(feed.rnd() * 120),
            };
            ws.tick(feed.name, TIMEFRAME, feed.last);
        }
    }, TICK_MS);
}

main().catch((error) => {
    console.error(error);
    show(`Çalışma alanı başlatılamadı: ${error?.message ?? error}\n\nDemo bir HTTP sunucusundan açılmalı (file:// çalışmaz), paket önce derlenmeli.`);
});
