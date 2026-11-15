// Parity check for the drawing-tool painter: replays every scenario of a
// `dump_paint` JSON on the ORIGINAL TypeScript `DrawingPainter`, painting on a
// recording context, and compares the canvas call stream (calls, order,
// arguments) with the one the Rust port recorded.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_paint.mjs <upstream checkout> <paint.json>
//
// Exits 1 when anything differs.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { RecordingContext } from './recording_canvas.mjs';
import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_paint.mjs <upstream checkout> <paint.json>');
    process.exit(2);
}

const core = useUpstream(upstreamArg);
const { createDrawing } = await core('drawings/registry.ts');
const { DrawingPainter } = await import(pathToFileURL(path.join(path.resolve(upstreamArg), 'src/renderers/native/drawings/DrawingPainter.ts')).href);

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));
const P = dump.projector;
const ID = dump.id;
const bars = dump.bars.map((b) => ({ time: b.time, open: b.open, high: b.high, low: b.low, close: b.close, ...(b.volume != null ? { volume: b.volume } : {}) }));

/** The same linear projector the Rust side used. */
const proj = {
    xOf: (time) => (time - P.timeOrigin) / P.msPerPx,
    yOf: (price, paneId) => (paneId === P.paneId ? (P.priceTop - price) / P.pricePerPx : null),
    pxToPoint: (x, y) => ({ time: P.timeOrigin + x * P.msPerPx, price: P.priceTop - y * P.pricePerPx }),
    paneIdAtY: (y) => (y >= 0 && y <= P.height ? P.paneId : null),
    paneRect: (paneId) => (paneId === P.paneId ? { top: 0, height: P.height } : null),
    barsBetween: (t1, t2) => Math.round((t2 - t1) / P.msPerPx),
    barsInRange: (from, to) => bars.filter((b) => b.time >= from && b.time <= to),
    width: P.width,
    height: P.height,
};

function paint(type, s) {
    const d = createDrawing(type, {
        id: ID,
        paneId: P.paneId,
        anchors: s.anchors.map((a) => ({ ...a })),
        createdAt: 0,
        ...(s.text ? { text: structuredClone(s.text) } : {}),
    });
    if (s.patch) d.applySettings({ [s.patch.path]: s.patch.value });
    if (s.placed) d.onPlaced(proj);
    const ctx = new RecordingContext({ width: P.width, height: P.height });
    const painter = new DrawingPainter();
    if (s.ghost) painter.paintGhost(ctx, d, proj, dump.theme);
    else painter.paintAll(ctx, [d], proj, dump.theme, { selected: new Set(s.selected ? [ID] : []), hovered: s.hovered ? ID : null });
    return ctx.toJSON();
}

const describe = (s) =>
    [
        `${s.anchors.length} anchors`,
        s.ghost ? 'ghost' : null,
        s.selected ? 'selected' : null,
        s.hovered ? 'hovered' : null,
        s.placed ? 'placed' : null,
        s.text ? `text ${JSON.stringify(s.text.value.slice(0, 12))}` : null,
        s.patch ? `${s.patch.path}=${JSON.stringify(s.patch.value)}` : null,
    ]
        .filter(Boolean)
        .join(', ');

const differ = new Differ();
const failures = [];
const failedTypes = new Set();
let scenarios = 0;
let calls = 0;
for (const t of dump.types) {
    for (const s of t.scenarios) {
        scenarios += 1;
        let original;
        try {
            original = paint(t.type, s);
        } catch (err) {
            failures.push(`${t.type} [${describe(s)}]: the original threw ${err?.stack ?? err}`);
            failedTypes.add(t.type);
            continue;
        }
        calls += original.length;
        const d = differ.diff(original, s.ops, '');
        if (d) {
            // Show the calls around the first difference.
            const at = Number(/^\[(\d+)\]/.exec(d)?.[1] ?? 0);
            const near = (ops) => JSON.stringify(ops.slice(Math.max(0, at - 2), at + 2));
            failures.push(`${t.type} [${describe(s)}] ${d}\n      original ${near(original)}\n      rust     ${near(s.ops)}`);
            failedTypes.add(t.type);
        }
    }
}

console.log(`${dump.types.length} drawing types, ${scenarios} painted scenarios, ${calls} canvas calls, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}`);
if (failures.length > 0) {
    console.log(`${failures.length} scenario(s) differ, in ${failedTypes.size} type(s): ${[...failedTypes].join(', ')}`);
    const shown = new Map();
    for (const f of failures) {
        const type = f.split(' ')[0];
        const n = shown.get(type) ?? 0;
        if (n < 2) console.log(`  ${f.length > 900 ? `${f.slice(0, 900)}…` : f}`);
        shown.set(type, n + 1);
    }
    process.exit(1);
}
console.log('The Rust port paints what the original paints.');
