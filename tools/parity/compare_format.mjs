// Parity check for translation and formatting: runs the same requests through
// the ORIGINAL TypeScript and through the Rust port compiled to WebAssembly,
// both on this Node's own `Intl`, and compares every answer.
//
// Usage (Node 23.6+), after building the WebAssembly package (see the README):
//   node tools/parity/compare_format.mjs <upstream checkout> <dir with vela_web.js>
//
// Exits 1 when anything differs.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { Differ, useUpstream } from './upstream.mjs';

const [upstreamArg, pkgArg] = process.argv.slice(2);
if (!upstreamArg || !pkgArg) {
    console.error('usage: compare_format.mjs <upstream checkout> <dir with vela_web.js>');
    process.exit(2);
}

// ── the Rust side ──
const pkg = path.resolve(pkgArg);
const wasm = await import(pathToFileURL(path.join(pkg, 'vela_web.js')).href);
await wasm.default({ module_or_path: readFileSync(path.join(pkg, 'vela_web_bg.wasm')) });

// ── the original side ──
const core = useUpstream(upstreamArg);
const { createTranslator, formatSettingsOf } = await core('i18n/translator.ts');
const { registerLocale } = await core('i18n/registry.ts');
const { fallbackChain } = await core('i18n/locale.ts');
const { upperCase } = await core('i18n/text.ts');
const { monthNames, weekdayNames, yearLabel, plainInteger, yearBeforeMonth } = await core('format/calendar.ts');
const { zoneOffsetMs } = await core('format/dates.ts');
const { tzOffset } = await core('timezones.ts');

const localesDir = path.join(path.resolve(upstreamArg), 'src/locales');
const packTags = readdirSync(localesDir)
    .filter((f) => f.endsWith('.json'))
    .map((f) => f.slice(0, -5))
    .sort();
for (const tag of packTags) {
    const messages = JSON.parse(readFileSync(path.join(localesDir, `${tag}.json`), 'utf8'));
    // The wrappers of right-to-left packs set the direction; read it off the wrapper source.
    const direction = /direction:\s*'rtl'/.test(readFileSync(path.join(localesDir, `${tag}.ts`), 'utf8')) ? 'rtl' : undefined;
    registerLocale({ locale: tag, messages, ...(direction ? { direction } : {}) });
}
const english = JSON.parse(readFileSync(path.join(localesDir, 'en.json'), 'utf8'));

// ── the requests ──
const requests = [];
const at = (iso) => Date.parse(iso);
const TIMES = [at('2026-01-01T00:00:00Z'), at('2026-09-15T13:05:09Z'), at('2024-02-29T23:59:59Z'), at('2025-03-30T01:30:00Z')];
const ZONES = [undefined, 'UTC', 'America/New_York', 'Asia/Tokyo', 'Europe/Istanbul', 'Asia/Kolkata', 'Not/AZone'];
const extraLocales = ['en-GB', 'en-IN', 'de-CH', 'fr-CA', 'es-MX', 'fa', 'zh', 'pt', 'sr-Latn', 'not a tag', ''];

function configsFor(locale) {
    const configs = [{}, { hourCycle: 'h12' }, { hourCycle: 'locale' }];
    if (['ar', 'hi', 'th', 'fa'].includes(locale)) configs.push({ numberingSystem: 'locale' }, { numberingSystem: 'arab', hourCycle: 'locale' });
    return configs.map((c) => ({ locale, ...c }));
}

function formattingOps(cfg) {
    const ops = [{ op: 'info' }, { op: 'weekdayNames' }, { op: 'yearBeforeMonth' }, { op: 'yearLabel', year: 2026 }, { op: 'plainInteger', n: 2026 }, { op: 'upper', text: 'istanbul ılık straße' }];
    for (const width of ['long', 'short']) ops.push({ op: 'monthNames', width });
    for (const value of [0, 1, -1, 1234.5678, -0.000123, 1e9, 0.1 + 0.2, 123456789.987, 0.5, 2.675, null]) {
        for (const decimals of [0, 2, 5]) ops.push({ op: 'price', value, decimals });
        ops.push({ op: 'price', value, decimals: 2, grouping: false });
    }
    for (const value of [0, 0.1234, -5.5, 150, 99.999, null]) {
        for (const signed of [true, false]) ops.push({ op: 'percent', value, decimals: 2, signed });
        ops.push({ op: 'percent', value });
    }
    for (const value of [0, 999, 1500, 123456, 1.2e6, 9.99e9, 1.5e12, -2500, 1e15, null]) {
        for (const trimZeros of [true, false]) ops.push({ op: 'compact', value, decimals: 2, trimZeros });
        ops.push({ op: 'compact', value });
    }
    for (const ms of [0, 999, 59_000, 3_600_000, 90_061_000, -5000, 86_400_000 * 40, null]) {
        for (const style of ['countdown', 'span', undefined]) ops.push({ op: 'duration', ms, style });
    }
    for (const ms of TIMES) {
        for (const timeZone of ZONES) {
            for (const unit of ['year', 'month', 'day', 'hour', 'minute', 'second']) for (const major of [true, false]) ops.push({ op: 'timeTick', ms, unit, major, timeZone });
            for (const withTime of [true, false]) ops.push({ op: 'timeStamp', ms, withTime, timeZone });
            for (const seconds of [true, false]) ops.push({ op: 'clock', ms, timeZone, seconds });
            if (timeZone) ops.push({ op: 'tzOffset', ms, zone: timeZone }, { op: 'zoneOffset', ms, zone: timeZone });
        }
        ops.push({ op: 'timeTick', ms, unit: 'hour', major: false, offsetMs: 3 * 3_600_000 });
    }
    return ops.map((o) => ({ ...cfg, ...o }));
}

for (const locale of [...packTags, ...extraLocales]) for (const cfg of configsFor(locale)) requests.push(...formattingOps(cfg));

// Every catalog message in every pack's language, with each argument bound to a number
// (three passes, to reach several plural branches) and once to text.
const argNames = (pattern) => [...new Set([...pattern.matchAll(/\{\s*([A-Za-z_][\w]*)/g)].map((m) => m[1]))];
for (const locale of packTags) {
    for (const [id, text] of Object.entries(english)) {
        const names = argNames(text);
        if (names.length === 0) {
            requests.push({ locale, op: 't', id, text, args: {} });
            continue;
        }
        for (const value of [1, 5, 1234.5, 'x']) requests.push({ locale, op: 't', id, text, args: Object.fromEntries(names.map((n) => [n, value])) });
    }
}

// ── run both sides ──
const rust = JSON.parse(wasm.parity_run(JSON.stringify(requests)));

const translators = new Map();
function translatorFor(r) {
    const key = JSON.stringify([r.locale ?? null, r.hourCycle ?? null, r.numberingSystem ?? null]);
    let t = translators.get(key);
    if (!t) {
        t = createTranslator({ locale: r.locale, ...(r.hourCycle ? { hourCycle: r.hourCycle } : {}), ...(r.numberingSystem ? { numberingSystem: r.numberingSystem } : {}) });
        translators.set(key, t);
    }
    return t;
}
const n = (v) => (v === null || v === undefined ? Number.NaN : v);
const defined = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined && v !== null));

function original(r) {
    const t = translatorFor(r);
    const settings = formatSettingsOf(t);
    switch (r.op) {
        case 'info':
            return { locale: t.locale, direction: t.direction, numberingSystem: t.numberingSystem, hourCycle: t.hourCycle, weekStart: t.weekStart, fallbackChain: fallbackChain(t.locale) };
        case 't':
            return t.t({ id: r.id, defaultMessage: r.text }, r.args);
        case 'price':
            return t.format.price(n(r.value), defined({ decimals: r.decimals, grouping: r.grouping, mintick: r.mintick }));
        case 'percent':
            return t.format.percent(n(r.value), defined({ decimals: r.decimals, signed: r.signed }));
        case 'compact':
            return t.format.compact(n(r.value), defined({ decimals: r.decimals, trimZeros: r.trimZeros }));
        case 'timeTick':
            return t.format.timeTick(n(r.ms), defined({ unit: r.unit, major: r.major, timeZone: r.timeZone, offsetMs: r.offsetMs }));
        case 'timeStamp':
            return t.format.timeStamp(n(r.ms), defined({ withTime: r.withTime, timeZone: r.timeZone }));
        case 'duration':
            return t.format.duration(n(r.ms), defined({ style: r.style }));
        case 'clock':
            return t.format.clock(n(r.ms), defined({ timeZone: r.timeZone, seconds: r.seconds }));
        case 'monthNames':
            return monthNames(settings, r.width);
        case 'weekdayNames':
            return weekdayNames(settings);
        case 'yearLabel':
            return yearLabel(settings, r.year);
        case 'plainInteger':
            return plainInteger(settings, r.n);
        case 'yearBeforeMonth':
            return yearBeforeMonth(settings);
        case 'tzOffset':
            return tzOffset(r.zone, new Date(r.ms));
        case 'zoneOffset':
            return zoneOffsetMs(r.ms, r.zone);
        case 'upper':
            return upperCase(r.text, t.locale);
        default:
            throw new Error(`unknown op ${r.op}`);
    }
}

const differ = new Differ();
const failures = new Map();
let threw = 0;
requests.forEach((r, i) => {
    let expected;
    try {
        expected = original(r);
    } catch (err) {
        threw += 1;
        expected = { threw: String(err) };
    }
    const d = differ.diff(expected, rust[i], JSON.stringify(r).slice(0, 260));
    if (d) {
        const bucket = failures.get(r.op) ?? [];
        bucket.push(d);
        failures.set(r.op, bucket);
    }
});

const total = [...failures.values()].reduce((sum, list) => sum + list.length, 0);
console.log(`${requests.length} requests across ${translators.size} translator configurations (${packTags.length} locale packs); the original threw on ${threw}`);
if (total > 0) {
    console.log(`${total} difference(s):`);
    for (const [op, list] of failures) {
        console.log(`  ${op}: ${list.length}`);
        for (const f of list.slice(0, 12)) console.log(`    ${f.length > 520 ? `${f.slice(0, 520)}…` : f}`);
    }
    process.exit(1);
}
console.log('The Rust port matches the original.');
