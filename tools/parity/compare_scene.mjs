// Parity check for the indicator-drawing painters: replays every scenario of a
// `dump_scene` JSON on the ORIGINAL TypeScript `DrawingSceneRenderer` and
// `paintTable`, painting on a recording context, and compares the canvas call
// streams, the tooltip regions and the autoscale price ranges with what the
// Rust port produced.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_scene.mjs <upstream checkout> <scene.json>
//
// Exits 1 when anything differs.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { RecordingContext } from './recording_canvas.mjs';
import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_scene.mjs <upstream checkout> <scene.json>');
    process.exit(2);
}

useUpstream(upstreamArg);
const shared = (file) => import(pathToFileURL(path.join(path.resolve(upstreamArg), 'src/renderers/shared', file)).href);
const { DrawingSceneRenderer } = await shared('DrawingSceneRenderer.ts');
const { paintTable } = await shared('TableCanvasRenderer.ts');

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));
const MINUTE = 60000;

/** Rust writes an absent optional as null; the original expects it missing. */
function dropNulls(v) {
    if (Array.isArray(v)) return v.map((x) => (x === null ? null : dropNulls(x)));
    if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).filter(([, x]) => x !== null).map(([k, x]) => [k, dropNulls(x)]));
    return v;
}
const plain = (v) => JSON.parse(JSON.stringify(v ?? null));

const deps = {
    timeToLogical: (ms) => ms / MINUTE,
    barAt: (logical) => {
        const i = Math.round(logical);
        return i >= 0 && i < dump.bars ? { high: 100 + (i % 17), low: 90 - (i % 11) } : null;
    },
    theme: dump.theme,
};
const xOf = (logical) => (logical - 20) * 6;
const yOf = (price) => (Number.isFinite(price) ? (150 - price) * 4 : null);

const differ = new Differ();
const failures = [];
let calls = 0;
function check(original, rust, what, ops) {
    const d = differ.diff(original, rust, '');
    if (!d) return;
    let detail = '';
    if (ops) {
        const at = Number(/^\[(\d+)\]/.exec(d)?.[1] ?? 0);
        const near = (list) => JSON.stringify(list.slice(Math.max(0, at - 3), at + 2));
        detail = `\n      original ${near(original)}\n      rust     ${near(rust)}`;
    }
    failures.push(`${what} ${d}${detail}`);
}

dump.scenes.forEach((s, n) => {
    try {
        const renderer = new DrawingSceneRenderer(deps);
        renderer.setSet(dropNulls(s.set), s.indexOffset);
        const ctx = new RecordingContext({ width: dump.width, height: dump.height });
        renderer.render(ctx, dump.width, dump.height, xOf, yOf);
        const ops = ctx.toJSON();
        calls += ops.length;
        check(ops, s.ops, `scene ${n} calls`, true);
        check(plain(renderer.labelTipRegions()), s.tips, `scene ${n} tip regions`);
        for (const r of s.ranges) check(plain(renderer.priceRange(r.from, r.to)), r.range, `scene ${n} price range ${r.from}..${r.to}`);
    } catch (err) {
        failures.push(`scene ${n}: the original threw ${err?.stack ?? err}`);
    }
});

dump.tables.forEach((t, n) => {
    try {
        const ctx = new RecordingContext({ width: dump.width, height: dump.height });
        const tips = [];
        paintTable(ctx, dropNulls(t.table), { paneHeight: dump.height, plotWidth: dump.tablePlotWidth, theme: dump.theme }, tips);
        const ops = ctx.toJSON();
        calls += ops.length;
        check(ops, t.ops, `table ${n} calls`, true);
        check(plain(tips), t.tips, `table ${n} tip regions`);
    } catch (err) {
        failures.push(`table ${n}: the original threw ${err?.stack ?? err}`);
    }
});

console.log(`${dump.scenes.length} drawing sets, ${dump.tables.length} tables, ${calls} canvas calls, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}`);
if (failures.length > 0) {
    console.log(`${failures.length} difference(s):`);
    for (const f of failures.slice(0, 25)) console.log(`  ${f.length > 1100 ? `${f.slice(0, 1100)}…` : f}`);
    process.exit(1);
}
console.log('The Rust port paints what the original paints.');
