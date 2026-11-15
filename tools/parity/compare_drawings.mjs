// Parity check for the drawing tools: replays every scenario of a
// `dump_drawings` JSON against the ORIGINAL TypeScript drawings and compares
// registry entries, geometry, hit-tests, drags, settings and serialization
// with what the Rust port produced.
//
// Usage (Node 23.6+):
//   node tools/parity/compare_drawings.mjs <upstream checkout> <drawings.json>
//
// Exits 1 when anything differs.

import { readFileSync } from 'node:fs';

import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, dumpArg] = process.argv.slice(2);
if (!upstreamArg || !dumpArg) {
    console.error('usage: compare_drawings.mjs <upstream checkout> <drawings.json>');
    process.exit(2);
}

const core = useUpstream(upstreamArg);
const { createDrawing, drawingTypeEntries } = await core('drawings/registry.ts');
const { sourceText } = await core('i18n/text.ts');

const dump = JSON.parse(readFileSync(dumpArg, 'utf8'));
const P = dump.projector;
const TOL = dump.tolerance;
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

const text = (m) => (m == null ? null : sourceText(m));
const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : v == null ? null : typeof v === 'number' ? null : v);
/** Deep-normalize: non-finite numbers and undefined become null, as in the Rust JSON. */
function plain(v) {
    if (v === undefined || v === null) return null;
    if (typeof v === 'number') return Number.isFinite(v) ? v : null;
    if (Array.isArray(v)) return v.map(plain);
    if (typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, plain(x)]));
    return v;
}

function schemaOf(s) {
    return {
        fields: s.fields.map((f) => ({
            path: f.path,
            label: text(f.label),
            kind: f.kind,
            min: num(f.min),
            max: num(f.max),
            step: num(f.step),
            options: f.options ? f.options.map((o) => ({ value: o.value, label: text(o.label) })) : null,
            group: f.group ?? null,
            when: f.when ? { path: f.when.path, in: plain([...f.when.in]) } : null,
            fallback: f.fallback ?? null,
            rowLabel: text(f.rowLabel),
        })),
        textIsContent: s.textIsContent === true,
    };
}

// Deliberate deviation: a `showOn` SETTING is stored as given by the original and in
// band order by the port (both reload into band order), so compare it as a set.
const bandSet = (keys) => (Array.isArray(keys) ? [...keys].sort() : null);
function sortRustShowOn(v) {
    if (Array.isArray(v)) v.forEach(sortRustShowOn);
    else if (v && typeof v === 'object') {
        if (Array.isArray(v.showOn)) v.showOn = bandSet(v.showOn);
        Object.values(v).forEach(sortRustShowOn);
    }
}

sortRustShowOn(dump.types);

function stateOf(d) {
    const doc = d.serialize();
    return plain({ anchors: doc.anchors, style: doc.style, text: doc.text ?? null, locked: doc.locked, visible: doc.visible, props: doc.props ?? null, showOn: bandSet(doc.showOn) });
}

function probe(d) {
    const handles = d.handlePoints(proj);
    let hits = '';
    const grabbed = [];
    for (let gy = 0; gy <= 12; gy++) {
        for (let gx = 0; gx <= 20; gx++) {
            const px = gx * 50;
            const py = gy * 50;
            hits += d.hitTest(px, py, proj, TOL) ? '1' : '0';
            grabbed.push(d.hitHandle(px, py, proj, TOL));
        }
    }
    const schema = d.anchorSchema();
    return plain({
        anchorSchema: { min: schema.min, max: schema.max, slots: schema.slots.map((s) => ({ role: s.role, free: s.free })) },
        isComplete: d.isComplete(),
        placementMode: d.placementMode(),
        handlePoints: handles.map((h) => [h[0], h[1]]),
        bounds: d.bounds(proj),
        priceRange: d.priceRange(),
        timeExtent: d.timeExtent(),
        hitGrid: hits,
        handleGrid: grabbed,
        hitHandleAtHandles: handles.map((h) => d.hitHandle(h[0] + 1, h[1] - 1, proj, TOL)),
        hitTestAtHandles: handles.map((h) => d.hitTest(h[0], h[1], proj, TOL)),
        schema: schemaOf(d.schema()),
        placeholderText: text(d.placeholderText()),
        editableLevels: d.editableLevels(),
        state: stateOf(d),
    });
}

function create(type, anchors, props) {
    return createDrawing(type, { id: 'dw-1', paneId: P.paneId, anchors: anchors.map((a) => ({ ...a })), createdAt: 0, ...(props ? { props: structuredClone(props) } : {}) });
}

const differ = new Differ();
const failures = [];
const check = (original, rust, at) => {
    const d = differ.diff(original, rust, at);
    if (d) failures.push(d);
};

const originalTypes = drawingTypeEntries().map((m) => m.type);
check(originalTypes, dump.types.map((t) => t.type), 'registration order');

let scenarios = 0;
let stricter = 0;
const GARBAGE = 'not the right type';
for (const meta of drawingTypeEntries()) {
    const rust = dump.types.find((t) => t.type === meta.type);
    if (!rust) continue;
    const type = meta.type;
    try {
        check(
            plain({ group: meta.group, label: text(meta.label), icon: meta.icon, defaultStyle: meta.defaultStyle, coversSeries: meta.coversSeries === true, placementHint: text(meta.placementHint) }),
            rust.meta,
            `${type} meta`,
        );
        for (const run of rust.runs) {
            const n = run.anchors.length;
            const d = create(type, run.anchors);
            scenarios += 1;
            check(probe(d), run.probe, `${type} [${n} anchors] probe`);
            check(plain(d.translateBody(120000, -3.5, run.anchors.map((a) => ({ ...a })))), run.translated, `${type} [${n} anchors] translated`);
            const drags = d.handlePoints(proj).map((_, h) => {
                const clone = create(type, run.anchors, d.serialize().props);
                clone.dragHandle(h, { time: 480 * 60000, price: 111 });
                clone.constrainHandleDrag(h);
                return plain({ handle: h, anchors: clone.anchors, handlePoints: clone.handlePoints(proj).map((p) => [p[0], p[1]]) });
            });
            check(drags, run.drags, `${type} [${n} anchors] drags`);
            const placed = create(type, run.anchors);
            placed.onPlaced(proj);
            check(stateOf(placed), run.placed, `${type} [${n} anchors] placed`);
        }
        const complete = rust.runs.find((r) => r.probe.isComplete)?.anchors ?? [];
        for (const s of rust.settings) {
            scenarios += 1;
            const d = create(type, complete.slice(0, rust.runs[0].probe.anchorSchema.min));
            d.applySettings({ [s.path]: s.value });
            check(probe(d), s.probe, `${type} setting ${s.path}=${JSON.stringify(s.value)}`);
        }
        scenarios += 1;
        const lenient = create(type, complete.slice(0, rust.runs[0].probe.anchorSchema.min), rust.garbageProps);
        const lenientState = stateOf(lenient);
        // Deliberate deviation: a few originals spread saved props over their defaults
        // unchecked, so a mistyped or unknown entry is stored as is. The port type-checks
        // every entry it reads and keeps the default instead.
        for (const [key, value] of Object.entries(lenientState.props ?? {})) {
            const ported = rust.lenient?.props?.[key] ?? null;
            const garbage = value === GARBAGE || key === '__unknown';
            if (garbage && JSON.stringify(ported) !== JSON.stringify(value)) {
                stricter += 1;
                if (ported === null) delete lenientState.props[key];
                else lenientState.props[key] = ported;
            }
        }
        check(lenientState, rust.lenient, `${type} garbage props`);
    } catch (err) {
        failures.push(`${type}: the original threw ${err?.stack ?? err}`);
    }
}

console.log(`${originalTypes.length} drawing types, ${scenarios} scenarios, ${differ.compared} numbers compared, largest relative error ${differ.maxError.toExponential(2)}`);
if (stricter > 0) console.log(`${stricter} mistyped or unknown saved prop(s) the original stores as is and the port rejects (deliberate)`);
if (failures.length > 0) {
    console.log(`${failures.length} difference(s):`);
    for (const f of failures.slice(0, 200)) console.log(`  ${f.length > 420 ? `${f.slice(0, 420)}…` : f}`);
    process.exit(1);
}
console.log('The Rust port matches the original.');
