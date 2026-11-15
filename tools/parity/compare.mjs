// Parity check: runs the ORIGINAL TypeScript classic catalog over the bars and
// input scenarios in a `dump_catalog` JSON, and compares every spec's
// metadata, inputs and compute output with what the Rust port produced.
//
// Usage (Node 23.6+; the upstream sources run through Node's own TypeScript support):
//   node tools/parity/compare.mjs <upstream checkout> <catalog.json>
//
// Exits 1 when anything differs.

import { readFileSync } from 'node:fs';

import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare.mjs <upstream checkout> <catalog.json>');
    process.exit(2);
}

const core = useUpstream(upstreamArg);
const { classicSpecs } = await core('native-indicators/classics/index.ts');
const { sourceText } = await core('i18n/text.ts');

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));

const text = (m) => (m == null ? null : sourceText(m));
const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const orNull = (v) => (v === undefined ? null : v);

function normalizeInput(i) {
    const when = i.when == null ? [] : Array.isArray(i.when) ? i.when : [i.when];
    return {
        key: i.key,
        title: text(i.title),
        type: i.type,
        defval: i.defval,
        min: orNull(i.min),
        max: orNull(i.max),
        step: orNull(i.step),
        options: i.options ? [...i.options] : null,
        group: orNull(i.group),
        inline: orNull(i.inline),
        tooltip: text(i.tooltip),
        when: when.map((c) => ({ key: c.key, equals: orNull(c.equals), anyOf: c.anyOf ? [...c.anyOf] : null })),
    };
}

function normalizeOutput(out) {
    return {
        plots: out.plots.map((p) => ({
            key: p.key,
            title: text(p.title),
            displayTitle: text(p.displayTitle),
            kind: p.kind ?? 'line',
            color: p.color,
            width: orNull(p.width),
            lineStyle: orNull(p.lineStyle),
            base: orNull(p.base),
            overlay: p.overlay === true,
            display: p.display
                ? { pane: orNull(p.display.pane), priceScale: orNull(p.display.priceScale), legend: orNull(p.display.legend), dataWindow: orNull(p.display.dataWindow) }
                : null,
            values: Array.from(p.values, num),
            colors: p.colors ? Array.from(p.colors, orNull) : null,
        })),
        bands: (out.bands ?? []).map((b) => ({
            key: b.key,
            from: b.from,
            to: b.to,
            color: b.color,
            colors: b.colors ? Array.from(b.colors, orNull) : null,
            gradient: b.gradient
                ? Array.from(b.gradient, (s) => (s == null ? null : { topValue: num(s.topValue), bottomValue: num(s.bottomValue), topColor: s.topColor, bottomColor: s.bottomColor }))
                : null,
        })),
        levels: (out.levels ?? []).map((l) => ({ key: l.key, price: num(l.price), color: orNull(l.color), lineStyle: orNull(l.lineStyle), title: text(l.title) })),
        markers: (out.markers ?? []).map((m) => ({ time: m.time, position: m.position, shape: m.shape, color: m.color, text: orNull(m.text), size: orNull(m.size) })),
        polylines: (out.polylines ?? []).map((p) => ({
            key: p.key,
            points: p.points.map((pt) => ({ xloc: pt.xloc, x: num(pt.x), price: num(pt.price) })),
            curved: p.curved,
            closed: p.closed,
            lineColor: orNull(p.lineColor),
            fillColor: orNull(p.fillColor),
            lineWidth: p.lineWidth,
            lineStyle: p.lineStyle,
            arrowLeft: p.arrowLeft,
            arrowRight: p.arrowRight,
            overlay: p.overlay === true,
        })),
    };
}

const differ = new Differ();
const diff = (a, b, at) => differ.diff(a, b, at);

const bars = Object.fromEntries(
    Object.entries(dump.bars).map(([name, set]) => [name, set.map((b) => ({ time: b.time, open: b.open, high: b.high, low: b.low, close: b.close, ...(b.volume != null ? { volume: b.volume } : {}) }))]),
);

const failures = [];
const originalTypes = classicSpecs.map((s) => s.type);
const rustTypes = dump.specs.map((s) => s.type);
const orderDiff = diff(originalTypes, rustTypes, 'catalog order');
if (orderDiff) failures.push(orderDiff);

let runs = 0;
for (const spec of classicSpecs) {
    const rust = dump.specs.find((s) => s.type === spec.type);
    if (!rust) continue;
    const meta = diff(
        { title: text(spec.title), shortTitle: text(spec.shortTitle), overlay: spec.overlay, inputs: spec.inputs.map(normalizeInput) },
        { title: rust.title, shortTitle: rust.shortTitle, overlay: rust.overlay, inputs: rust.inputs },
        spec.type,
    );
    if (meta) failures.push(meta);
    for (const run of rust.runs) {
        runs += 1;
        const label = `${spec.type} [${run.name} @ ${run.bars}]`;
        let original;
        try {
            original = normalizeOutput(spec.compute(bars[run.bars], run.inputs));
        } catch (err) {
            failures.push(`${label}: the original threw ${err}`);
            continue;
        }
        const d = diff(original, run.output, label);
        if (d) failures.push(d);
    }
}

console.log(`${classicSpecs.length} specs, ${runs} runs, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}`);
if (differ.knownDeviations > 0) console.log(`${differ.knownDeviations} color(s) where the original prints "#NaNNaNNaN" and the port a valid ink (deliberate)`);
if (failures.length > 0) {
    console.log(`${failures.length} difference(s):`);
    for (const f of failures.slice(0, 60)) console.log(`  ${f}`);
    process.exit(1);
}
console.log('The Rust port matches the original.');
