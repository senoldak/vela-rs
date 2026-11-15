// Parity check for the small pure core functions: replays the inputs of a
// `dump_misc` JSON against the ORIGINAL TypeScript (timeframes, Heikin Ashi,
// extended tickers, visible-range presets, symbol helpers, the state document
// sanitizer) and compares the answers with what the Rust port produced.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_misc.mjs <upstream checkout> <misc.json>
//
// Exits 1 when anything differs.

import { readFileSync } from 'node:fs';

import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_misc.mjs <upstream checkout> <misc.json>');
    process.exit(2);
}

const core = useUpstream(upstreamArg);
const { timeframeToMs } = await core('../data/timeframe.ts');
const { baseOf, ledgerCryptoIconUrl } = await core('../data/symbol-base.ts');
const { isGroupRow, groupKeyOf, groupMembers, defaultMemberOf } = await core('../data/symbol-groups.ts');
const { heikinAshiFull, heikinAshiNext } = await core('price-styles/heikin-ashi.ts');
const { parseExtendedTicker } = await core('price-styles/BarTransform.ts');
const { registerBuiltinChartTypes } = await core('../chart-types/builtins.ts');
const { presetToRange } = await core('visible-range.ts');
const { sanitizeState, encodeState } = await core('../state/document.ts');

registerBuiltinChartTypes();

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));
const toBars = (list) => list.map((b) => ({ time: b.time, open: b.open, high: b.high, low: b.low, close: b.close, ...(b.volume != null ? { volume: b.volume } : {}) }));
const barOut = (b) => ({ time: b.time, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume ?? null });

const differ = new Differ();
const failures = [];
let checks = 0;
const check = (original, rust, at) => {
    checks += 1;
    const d = differ.diff(original, rust, at);
    if (d) failures.push(d);
};
const guarded = (at, f) => {
    try {
        f();
    } catch (err) {
        failures.push(`${at}: the original threw ${err?.stack ?? err}`);
    }
};

for (const t of dump.timeframes) guarded(`timeframe ${JSON.stringify(t.input)}`, () => check(timeframeToMs(t.input), t.ms, `timeframe ${JSON.stringify(t.input)}`));

guarded('heikin ashi', () => {
    const raw = toBars(dump.heikinAshi.raw);
    const full = heikinAshiFull(raw);
    check(full.map(barOut), dump.heikinAshi.full, 'heikin ashi full');
    check(raw.map((r, i) => barOut(heikinAshiNext(r, i > 0 ? full[i - 1] : undefined))), dump.heikinAshi.stepped, 'heikin ashi stepped');
});

for (const t of dump.tickers) {
    guarded(`ticker ${JSON.stringify(t.input)}`, () => {
        const p = parseExtendedTicker(t.input);
        check({ input: t.input, symbol: p.symbol, modifier: p.modifier, hasTransform: p.transform != null }, t, `ticker ${JSON.stringify(t.input)}`);
    });
}

for (const run of dump.presets.runs) {
    guarded(`preset ${run.preset} on ${run.bars}`, () => {
        const range = presetToRange(run.preset, toBars(dump.presets.bars[run.bars]));
        check(range ? { from: range.from, to: range.to } : null, run.range, `preset ${run.preset} on ${run.bars}`);
    });
}

const pool = dump.symbols.pool;
dump.symbols.runs.forEach((run, i) => {
    guarded(`symbol ${run.ticker}`, () => {
        const d = pool[i];
        const base = baseOf(d);
        check(
            {
                ticker: d.ticker,
                base,
                icon: ledgerCryptoIconUrl(base) ?? null,
                isGroupRow: isGroupRow(d),
                groupKey: groupKeyOf(d),
                members: groupMembers(pool, d).map((m) => m.ticker),
                defaultMember: defaultMemberOf(pool, d)?.ticker ?? null,
            },
            run,
            `symbol #${i} ${run.ticker}`,
        );
    });
});

dump.states.forEach((s, i) => {
    guarded(`state #${i}`, () => {
        const sanitized = sanitizeState(structuredClone(s.doc));
        check(sanitized ? JSON.parse(encodeState(sanitized)) : null, s.sanitized, `state #${i}`);
    });
});

console.log(`${checks} checks, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}`);
if (failures.length > 0) {
    console.log(`${failures.length} difference(s):`);
    for (const f of failures.slice(0, 80)) console.log(`  ${f.length > 500 ? `${f.slice(0, 500)}…` : f}`);
    process.exit(1);
}
console.log('The Rust port matches the original.');
