// The original's embedding example for this build: README quick start, then the provider path.
// Each place where this build differs from the original has a comment starting "Fark:".

// Fark: the original imports from '@luxalgo/vela'; here the wasm package is loaded first.
import init, { Vela } from '../pkg/vela_web.js';

const message = document.getElementById('message');
const fail = (error) => {
    message.hidden = false;
    message.textContent = String(error?.stack ?? error);
    throw error;
};

try {
    await init();

    // Fark: the original's `myBars` come from your own data; these are synthetic and fixed-seed (no network).
    const HOUR = 3_600_000;
    const T0 = Date.UTC(2025, 0, 1);
    function makeBars(count, start, step, seed) {
        let s = seed;
        const rnd = () => ((s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
        const bars = [];
        let price = 100;
        for (let i = 0; i < count; i += 1) {
            const open = price;
            const close = open + (rnd() - 0.5) * 2;
            bars.push({ time: start + i * step, open, high: Math.max(open, close) + rnd(), low: Math.min(open, close) - rnd(), close, volume: 1000 + Math.floor(rnd() * 500) });
            price = close;
        }
        return bars;
    }
    const myBars = makeBars(400, T0, HOUR, 7);

    // 1. The quick start: offline data, no provider, no network.
    const chart = new Vela('#chart', { data: myBars, timeframe: '1h', theme: 'dark' });

    // Fark: the original registers a scripting engine (`chart.registerEngine('pine', new PineEngine())`) and adds
    // a script. This build has no scripting engine and `registerEngine` throws; use a native indicator.
    const rsi = chart.addNativeIndicator('rsi');
    rsi.on('ready', () => console.log(rsi.title, 'has computed'));

    await chart.ready();
    chart.on('market:changed', ({ symbol, timeframe }) => console.log('market', symbol, timeframe));

    // 2. Full control: a provider registered on the chart, a live chart.
    // Fark: the original registers `new BinanceProvider()`, which reaches the exchange. This provider is a plain
    // object with the same method names (`getBars`, `listSymbols`, `subscribe`) over synthetic bars: no network.
    const series = { ALFA: makeBars(500, T0, HOUR, 11), BETA: makeBars(500, T0, HOUR, 23) };
    const demoProvider = {
        async getBars(ticker, _timeframe, range) {
            const all = series[ticker] ?? [];
            const inRange = all.filter((b) => (range.from === undefined || b.time >= range.from) && (range.to === undefined || b.time <= range.to));
            return range.limit === undefined ? inRange : inRange.slice(-range.limit);
        },
        async listSymbols() {
            return Object.keys(series).map((ticker) => ({ ticker, description: `${ticker} (sentetik)` }));
        },
        subscribe(ticker, _timeframe, onBar) {
            const timer = setInterval(() => {
                const last = series[ticker].at(-1);
                const close = last.close + (Math.random() - 0.5);
                onBar({ ...last, close, high: Math.max(last.high, close), low: Math.min(last.low, close) });
            }, 500);
            return () => clearInterval(timer);
        },
    };

    const live = new Vela('#live', { symbol: 'demo:ALFA', timeframe: '1h', live: true, theme: 'dark' });
    live.data.registerProvider('demo', demoProvider); // registering it fires the parked first load
    await live.ready();

    // Fark: `chart.drawings.add` returns the drawing's record ({ id, type, anchors, ... }), not a class instance.
    live.drawings.add('hline', { anchors: [{ time: series.ALFA.at(-1).time, price: series.ALFA.at(-1).close }] });

    // T switches the theme, S switches the symbol of the live chart (through setMarket, as in the original).
    let symbol = 'ALFA';
    let theme = 'dark';
    window.addEventListener('keydown', (event) => {
        if (event.key === 't') {
            theme = theme === 'dark' ? 'light' : 'dark';
            chart.setTheme(theme);
            live.setTheme(theme);
        }
        if (event.key === 's') {
            symbol = symbol === 'ALFA' ? 'BETA' : 'ALFA';
            live.setMarket({ symbol: `demo:${symbol}` });
        }
    });
    window.velaChart = chart;
    window.velaLive = live;
} catch (error) {
    fail(error);
}
