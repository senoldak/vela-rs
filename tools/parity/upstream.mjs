// Loads modules of the ORIGINAL TypeScript sources straight from an upstream
// checkout, through Node's own type stripping (Node 23.6+). No `npm install`.

import { readFileSync, existsSync } from 'node:fs';
import { registerHooks } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const UI_KIT = '@luxalgo/vela-ui/';

/**
 * Register the resolve/load hooks for an upstream checkout and return a helper
 * that imports a module under its `src/core`.
 */
export function useUpstream(upstreamDir) {
    const upstream = path.resolve(upstreamDir);
    registerHooks({
        // The upstream imports are extensionless and name the UI kit by its package name.
        resolve(specifier, context, nextResolve) {
            if (specifier.startsWith(UI_KIT)) {
                const file = path.join(upstream, 'packages/vela-ui/src/lib', `${specifier.slice(UI_KIT.length)}.ts`);
                return { url: pathToFileURL(file).href, shortCircuit: true };
            }
            if (specifier.startsWith('.') && context.parentURL?.startsWith('file:') && !path.extname(specifier)) {
                const base = path.resolve(path.dirname(fileURLToPath(context.parentURL)), specifier);
                for (const candidate of [`${base}.ts`, path.join(base, 'index.ts')]) {
                    if (existsSync(candidate)) return { url: pathToFileURL(candidate).href, shortCircuit: true };
                }
            }
            return nextResolve(specifier, context);
        },
        // Node only strips types. Constructor parameter properties need real code, so
        // rewrite `constructor(private readonly x: T) {` into an explicit assignment.
        load(url, context, nextLoad) {
            if (!url.startsWith('file:') || !url.endsWith('.ts')) return nextLoad(url, context);
            const source = rewriteParameterProperties(readFileSync(fileURLToPath(url), 'utf8'));
            return { format: 'module-typescript', source: declareBareFields(source), shortCircuit: true };
        },
    });
    return (rel) => import(pathToFileURL(path.join(upstream, 'src/core', rel)).href);
}

/**
 * `constructor(private readonly x: T, y: U) {` → `constructor(x: T, y: U) { (this as any).x = x;`.
 * The parameter list is scanned with its brackets balanced, since a parameter's type may
 * itself hold parentheses (a function type) or commas (generics, object types).
 */
function rewriteParameterProperties(source) {
    const MODIFIERS = /^((?:\s|\/\*[\s\S]*?\*\/|\/\/[^\n]*\n)*)((?:(?:private|public|protected|readonly|override)\s+)+)(\w+)/;
    let out = '';
    let from = 0;
    for (;;) {
        const at = source.indexOf('constructor(', from);
        if (at < 0) break;
        const open = at + 'constructor('.length;
        let depth = 1;
        let i = open;
        const cuts = [open];
        for (; i < source.length && depth > 0; i++) {
            const c = source[i];
            if (c === '(' || c === '[' || c === '{' || c === '<') depth += 1;
            else if (c === ')' || c === ']' || c === '}') depth -= 1;
            else if (c === '>' && source[i - 1] !== '=') depth -= 1;
            else if (c === ',' && depth === 1) cuts.push(i + 1);
        }
        const close = i - 1;
        const body = /^\s*\{/.exec(source.slice(close + 1));
        if (depth !== 0 || !body) {
            out += source.slice(from, open);
            from = open;
            continue;
        }
        cuts.push(close + 1);
        const names = [];
        let params = '';
        for (let k = 0; k + 1 < cuts.length; k++) {
            const param = source.slice(cuts[k], cuts[k + 1] - 1);
            const m = MODIFIERS.exec(param);
            if (m) names.push(m[3]);
            params += (m ? m[1] + m[3] + param.slice(m[0].length) : param) + (k + 2 < cuts.length ? ',' : '');
        }
        const end = close + 1 + body[0].length;
        out += `${source.slice(from, open)}${params})${body[0]}${names.map((n) => ` (this as any).${n} = ${n};`).join('')}`;
        from = end;
    }
    return out + source.slice(from);
}

/**
 * Upstream compiles for ES2020, where a class field declared without an initializer
 * (`reverse!: boolean;`) emits no code. Node's type stripping instead keeps it as a real
 * field, which resets it to `undefined` AFTER the base constructor has read the saved
 * props into it. Mark such fields `declare` so they vanish, as in upstream's own build.
 */
function declareBareFields(source) {
    let inClass = false;
    return source
        .split('\n')
        .map((line) => {
            if (/^(export\s+)?(default\s+)?(abstract\s+)?class\s/.test(line)) inClass = true;
            else if (/^}/.test(line)) inClass = false;
            if (!inClass) return line;
            const field = /^ {4}((?:(?:public|private|protected|readonly|override)\s+)*)(\w+)[!?]?:\s[^=;(]*;\s*(\/\/.*)?$/.exec(line);
            return field ? `    declare ${line.slice(4)}` : line;
        })
        .join('\n');
}

/** Compares normalized values, counting numbers and tolerating float noise. */
export class Differ {
    constructor(tolerance = 1e-9) {
        this.tolerance = tolerance;
        this.compared = 0;
        this.maxError = 0;
        this.knownDeviations = 0;
    }

    /** First difference as a `path: description` string; null when equal. */
    diff(a, b, at = '') {
        if (typeof a === 'number' && typeof b === 'number') {
            this.compared += 1;
            const error = Math.abs(a - b) / Math.max(1, Math.abs(a), Math.abs(b));
            if (error > this.maxError) this.maxError = error;
            return error <= this.tolerance ? null : `${at}: original ${a} vs rust ${b}`;
        }
        if (Array.isArray(a) && Array.isArray(b)) {
            if (a.length !== b.length) return `${at}: length ${a.length} vs ${b.length}`;
            for (let i = 0; i < a.length; i++) {
                const d = this.diff(a[i], b[i], `${at}[${i}]`);
                if (d) return d;
            }
            return null;
        }
        if (a && b && typeof a === 'object' && typeof b === 'object' && !Array.isArray(a) && !Array.isArray(b)) {
            const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
            for (const k of keys) {
                // An absent key and a null one are the same thing across the two JSON writers.
                const d = this.diff(a[k] ?? null, b[k] ?? null, `${at}.${k}`);
                if (d) return d;
            }
            return null;
        }
        // Known, deliberate deviation: blending between NaN bounds, the original prints an
        // unparseable color; the port hands back the starting ink (see `shared::gradient`).
        if (a === '#NaNNaNNaN' && typeof b === 'string' && /^#[0-9a-f]{6}([0-9a-f]{2})?$/i.test(b)) {
            this.knownDeviations += 1;
            return null;
        }
        return Object.is(a ?? null, b ?? null) ? null : `${at}: original ${JSON.stringify(a)} vs rust ${JSON.stringify(b)}`;
    }
}
