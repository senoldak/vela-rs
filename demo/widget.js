// Vela (Rust) widget demo: the assembled single-chart widget (topbar, drawing toolbar, sidebar
// with the object tree and the data window, bottom bar, status line, watermark) on synthetic
// data, no network.
//
// Twelve invented symbols on two invented venues (NORTE and SUR) are generated here with fixed
// seeds, so every load shows the same history, and an in-memory provider serves them: the
// symbol search has something to search, the exchange providers are not used. The embedding
// passes the options a typical page passes (compare `playground/widget.ts` of the original):
// symbol, timeframe, theme, autofocus, persist and the shell parts.
//
// Query:
//   ?lang=tr     the Turkish language pack (any shipped tag works: de, fr, ja, ...)
//   ?theme=light the light theme (default dark)
//
// localStorage:
//   vela-demo-widget   the widget document (symbol, timeframe, indicators, drawings, panels)

import init, { VelaWidget } from '../pkg/vela_web.js';

const STATE_KEY = 'vela-demo-widget';
const BAR_COUNT = 300;
const START = Date.UTC(2025, 0, 1);
const TICK_MS = 500;
const TICKS_PER_BAR = 20;

// Timeframe -> bar length in ms. Each symbol carries one series per timeframe, so switching
// the timeframe shows different bars, not the same ones rescaled.
const TIMEFRAMES = {
    1: 60_000,
    5: 300_000,
    15: 900_000,
    30: 1_800_000,
    60: 3_600_000,
    240: 14_400_000,
    D: 86_400_000,
    W: 604_800_000,
};

// Invented instruments: the names mean nothing, the series are random walks.
const SYMBOLS = [
    { name: 'ALFA', venue: 'NORTE', type: 'stock', description: 'Alfa Holding', level: 100, seed: 20250101 },
    { name: 'BETA', venue: 'NORTE', type: 'stock', description: 'Beta Enerji', level: 60, seed: 77 },
    { name: 'GAMA', venue: 'NORTE', type: 'stock', description: 'Gama Madencilik', level: 230, seed: 4242 },
    { name: 'DELTA', venue: 'NORTE', type: 'stock', description: 'Delta Lojistik', level: 15, seed: 9001 },
    { name: 'EPSILON', venue: 'NORTE', type: 'stock', description: 'Epsilon Gıda', level: 42, seed: 31337 },
    { name: 'ZETA', venue: 'NORTE', type: 'stock', description: 'Zeta Teknoloji', level: 310, seed: 555 },
    { name: 'KAPPA', venue: 'SUR', type: 'crypto', description: 'Kappa / Dolar', level: 1800, seed: 1234 },
    { name: 'LAMBDA', venue: 'SUR', type: 'crypto', description: 'Lambda / Dolar', level: 45, seed: 8080 },
    { name: 'SIGMA', venue: 'SUR', type: 'crypto', description: 'Sigma / Dolar', level: 0.8, seed: 2718 },
    { name: 'OMEGA', venue: 'SUR', type: 'crypto', description: 'Omega / Dolar', level: 320, seed: 6174 },
    { name: 'TAU', venue: 'SUR', type: 'crypto', description: 'Tau / Dolar', level: 7, seed: 1618 },
    { name: 'ZENIT', venue: 'SUR', type: 'crypto', description: 'Zenit / Dolar', level: 95, seed: 4040 },
];

function lcg(seed) {
    let state = seed >>> 0;
    return () => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
        return state / 4294967296;
    };
}

// The step of a bar grows with its length: a daily bar moves more than a minute bar.
function makeBars({ level, seed }, step, scale) {
    const rnd = lcg(seed + step);
    const bars = [];
    let price = level;
    for (let i = 0; i < BAR_COUNT; i += 1) {
        const open = price;
        const drift = Math.sin(i / 37) * 0.0025 * level * scale;
        const close = Math.max(level * 0.2, open + drift + (rnd() - 0.5) * 0.024 * level * scale);
        const high = Math.max(open, close) + rnd() * 0.009 * level * scale;
        const low = Math.min(open, close) - rnd() * 0.009 * level * scale;
        bars.push({ time: START + i * step, open, high, low, close, volume: Math.round(800 + rnd() * 1200) });
        price = close;
    }
    return bars;
}

function seriesOf(symbol) {
    const out = {};
    for (const [timeframe, step] of Object.entries(TIMEFRAMES)) {
        out[timeframe] = makeBars(symbol, step, Math.sqrt(step / 3_600_000) * 0.6 + 0.4);
    }
    return out;
}

// localStorage can be missing or blocked: never fatal.
function read(key) {
    try {
        return localStorage.getItem(key);
    } catch {
        return null;
    }
}

function show(message) {
    const el = document.getElementById('message');
    el.textContent = message;
    el.hidden = false;
}

async function main() {
    await init();
    const query = new URLSearchParams(location.search);
    const lang = query.get('lang');
    const theme = query.get('theme') === 'light' ? 'light' : 'dark';
    const bars = Object.fromEntries(SYMBOLS.map((s) => [s.name, seriesOf(s)]));
    const symbols = Object.fromEntries(
        SYMBOLS.map((s) => [s.name, { description: s.description, type: s.type, prefix: s.venue }]),
    );

    const host = document.getElementById('widget');
    // The page is Turkish; the widget speaks English unless ?lang says otherwise, so mark its language (uppercase rules differ).
    host.setAttribute('lang', lang || 'en');
    const widget = new VelaWidget(host, {
        bars,
        symbols,
        symbol: SYMBOLS[0].name,
        timeframe: '60',
        theme,
        live: true,
        autofocus: true,
        persist: STATE_KEY,
        // The shell parts, at their defaults: the topbar, the bottom bar, the status line and
        // the watermark show; the sidebar is opt-in.
        sidebar: true,
        ...(lang ? { locale: lang } : {}),
        ...(lang === 'tr' ? { timezone: 'Europe/Istanbul' } : {}),
    });
    window.velaWidget = widget;

    // First visit: two indicators, so the legend and the object tree have something to show.
    await widget.ready();
    if (read(STATE_KEY) === null) {
        widget.addIndicator('ema', undefined);
        widget.addIndicator('rsi', undefined);
    }

    // A live feed stand-in for the symbol and timeframe on screen: each tick moves the last
    // bar's close; every TICKS_PER_BAR ticks the next bar opens. One walk per series.
    const walks = new Map();
    let ticks = 0;
    setInterval(() => {
        const symbol = widget.symbol().split(':').at(-1);
        const timeframe = widget.timeframe();
        const meta = SYMBOLS.find((s) => s.name === symbol);
        const step = TIMEFRAMES[timeframe];
        if (!meta || !step) return;
        const id = `${symbol}|${timeframe}`;
        if (!walks.has(id)) {
            walks.set(id, { rnd: lcg(7 + meta.seed), last: { ...bars[symbol][timeframe].at(-1) } });
        }
        const walk = walks.get(id);
        ticks += 1;
        if (ticks % TICKS_PER_BAR === 0) {
            const c = walk.last.close;
            walk.last = { time: walk.last.time + step, open: c, high: c, low: c, close: c, volume: 0 };
        }
        const close = Math.max(meta.level * 0.2, walk.last.close + (walk.rnd() - 0.5) * 0.008 * meta.level);
        walk.last = {
            ...walk.last,
            close,
            high: Math.max(walk.last.high, close),
            low: Math.min(walk.last.low, close),
            volume: walk.last.volume + Math.round(walk.rnd() * 120),
        };
        widget.tick(symbol, timeframe, walk.last);
    }, TICK_MS);
}

main().catch((error) => {
    console.error(error);
    show(`Pencere öğesi başlatılamadı: ${error?.message ?? error}\n\nDemo bir HTTP sunucusundan açılmalı (file:// çalışmaz), paket önce derlenmeli.`);
});
