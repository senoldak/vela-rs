// Vela (Rust) demo: one chart, synthetic data, no network.
//
//   ?mode=app   builds the chart through the Vela facade (engine + renderer) instead of the
//               direct renderer API; the volume layer is then on by default.
//   T           switches between the dark and the light theme.
//
// The drawing toolbar is docked on the left. Drawings are kept in localStorage under
// `vela-demo-drawings` and come back on reload (`velaChart.clearDrawings()` empties them).
// The gear above the time axis opens the chart settings dialog; the chart configuration is
// kept in localStorage under `vela-demo-config` (`getConfig()` / `applyConfig(json)`). The
// legend is not on this page yet: `velaChart.openIndicatorSettings(id)` opens an indicator's
// dialog.

import init, { VelaChart, VelaApp } from '../pkg/vela_web.js';

const HOUR = 3_600_000;
const BAR_COUNT = 400;
const START = Date.UTC(2025, 0, 1);
const TICK_MS = 500;
const TICKS_PER_BAR = 20;
const DRAWINGS_KEY = 'vela-demo-drawings';
const CONFIG_KEY = 'vela-demo-config';
const SAVE_EVERY_MS = 750;

// A small deterministic generator, so every load shows the same history.
function lcg(seed) {
    let state = seed >>> 0;
    return () => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
        return state / 4294967296;
    };
}

function makeBars(count) {
    const rnd = lcg(20250101);
    const bars = [];
    let price = 100;
    for (let i = 0; i < count; i += 1) {
        const open = price;
        const drift = Math.sin(i / 37) * 0.25;
        const close = Math.max(1, open + drift + (rnd() - 0.5) * 2.4);
        const high = Math.max(open, close) + rnd() * 0.9;
        const low = Math.min(open, close) - rnd() * 0.9;
        bars.push({ time: START + i * HOUR, open, high, low, close, volume: Math.round(800 + rnd() * 1200) });
        price = close;
    }
    return bars;
}

// localStorage can be missing or blocked (private windows, site data off): never fatal.
function readSaved() {
    try {
        return localStorage.getItem(DRAWINGS_KEY);
    } catch {
        return null;
    }
}

function writeSaved(text) {
    try {
        localStorage.setItem(DRAWINGS_KEY, text);
    } catch {
        // Not persisted; the chart goes on.
    }
}

// Restore the saved drawings, dock the toolbar, then keep the saved copy current. The chart
// has no change callback on this entry, so the export text is compared on a short timer
// (and once more when the page is hidden); it only writes when it changed.
function setUpDrawings(chart) {
    const saved = readSaved();
    if (saved) {
        try {
            chart.importDrawings(saved);
        } catch (error) {
            console.warn('Kaydedilmiş çizimler okunamadı:', error);
        }
    }
    chart.showDrawingToolbar(true);
    let last = chart.exportDrawings();
    const save = () => {
        const now = chart.exportDrawings();
        if (now !== last) {
            last = now;
            writeSaved(now);
        }
    };
    setInterval(save, SAVE_EVERY_MS);
    window.addEventListener('pagehide', save);
    document.addEventListener('visibilitychange', () => {
        if (document.visibilityState === 'hidden') save();
    });
}

// Restore the saved chart configuration, show the settings gear, and save the configuration
// whenever it changes (a settings edit or an applied document).
function setUpSettings(chart) {
    let saved = null;
    try {
        saved = localStorage.getItem(CONFIG_KEY);
    } catch {
        // No storage: the configuration is not kept.
    }
    if (saved) {
        try {
            chart.applyConfig(saved);
        } catch (error) {
            console.warn('Kaydedilmiş ayarlar okunamadı:', error);
        }
    }
    chart.showSettings(true);
    chart.onConfigChanged(() => {
        try {
            localStorage.setItem(CONFIG_KEY, chart.getConfig());
        } catch {
            // Not persisted; the chart goes on.
        }
    });
}

function show(message) {
    const el = document.getElementById('message');
    el.textContent = message;
    el.hidden = false;
}

async function main() {
    await init();
    const container = document.getElementById('chart');
    const bars = makeBars(BAR_COUNT);
    const useApp = new URLSearchParams(location.search).get('mode') === 'app';
    let theme = 'dark';

    let chart;
    let push;
    if (useApp) {
        chart = new VelaApp(container, theme, 'DEMO', '1h', bars);
        await chart.ready();
        chart.addIndicator('ema');
        chart.addIndicator('rsi');
        push = (bar) => chart.tick(bar.time, bar.open, bar.high, bar.low, bar.close, bar.volume);
    } else {
        chart = new VelaChart(container, theme);
        chart.setBars(bars);
        chart.addVolume();
        chart.addIndicator('ema');
        chart.addIndicator('rsi');
        push = (bar) => chart.updateBar(bar.time, bar.open, bar.high, bar.low, bar.close, bar.volume);
    }

    setUpSettings(chart);
    setUpDrawings(chart);

    // A live feed stand-in: each tick moves the last bar's close; every TICKS_PER_BAR ticks
    // the next hour opens.
    const rnd = lcg(7);
    let last = { ...bars[bars.length - 1] };
    let ticks = 0;
    setInterval(() => {
        ticks += 1;
        if (ticks % TICKS_PER_BAR === 0) {
            last = { time: last.time + HOUR, open: last.close, high: last.close, low: last.close, close: last.close, volume: 0 };
        }
        const close = Math.max(1, last.close + (rnd() - 0.5) * 0.8);
        last = {
            ...last,
            close,
            high: Math.max(last.high, close),
            low: Math.min(last.low, close),
            volume: last.volume + Math.round(rnd() * 120),
        };
        push(last);
    }, TICK_MS);

    window.addEventListener('keydown', (e) => {
        const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target?.tagName);
        if ((e.key === 't' || e.key === 'T') && !e.ctrlKey && !e.metaKey && !e.altKey && !typing) {
            theme = theme === 'dark' ? 'light' : 'dark';
            chart.setTheme(theme);
            document.body.style.background = theme === 'dark' ? '#151619' : '#ffffff';
        }
    });
    window.velaChart = chart;
}

main().catch((error) => {
    console.error(error);
    show(`Grafik başlatılamadı: ${error?.message ?? error}\n\nDemo bir HTTP sunucusundan açılmalı (file:// çalışmaz), paket önce derlenmeli.`);
});
