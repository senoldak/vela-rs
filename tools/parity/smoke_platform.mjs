// Smoke test of vela-web's `BrowserPlatform` (timers, fetch, abort, timeout)
// in Node, against a throwaway HTTP server on the loopback interface.
//
// Usage, after `cargo run -p xtask -- web pkg-parity --parity`:
//   node tools/parity/smoke_platform.mjs pkg-parity
//
// Exits 1 when the report differs from what the platform contract promises.

import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const pkgArg = process.argv[2];
if (!pkgArg) {
    console.error('usage: smoke_platform.mjs <dir with vela_web.js>');
    process.exit(2);
}
const pkg = path.resolve(pkgArg);
const wasm = await import(pathToFileURL(path.join(pkg, 'vela_web.js')).href);
await wasm.default({ module_or_path: readFileSync(path.join(pkg, 'vela_web_bg.wasm')) });

const hanging = new Set();
const server = createServer((req, res) => {
    if (req.url === '/hello') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('hello');
    } else if (req.url === '/echo' && req.method === 'POST') {
        let body = '';
        req.on('data', (chunk) => (body += chunk));
        req.on('end', () => {
            res.writeHead(200, { 'Content-Type': req.headers['content-type'] ?? 'none' });
            res.end(body);
        });
    } else if (req.url === '/slow') {
        hanging.add(res); // never answered: the client must abort or time out
        res.on('close', () => hanging.delete(res));
    } else {
        res.writeHead(404, { 'Content-Type': 'text/plain' });
        res.end('nope');
    }
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}`;

let report;
try {
    report = JSON.parse(await wasm.platform_smoke(base));
} finally {
    for (const res of hanging) res.destroy();
    server.close();
}

const expected = {
    sleptAtLeast40: true,
    clearedTimerFired: false,
    ok: { status: 200, ok: true, body: 'hello', contentType: 'text/plain' },
    posted: { status: 200, ok: true, body: '{"a":1}', contentType: 'application/json' },
    missing: { status: 404, ok: false, body: 'nope', contentType: 'text/plain' },
    aborted: { error: 'Aborted' },
    timedOut: { error: 'Timeout' },
    refusedIsFailure: true,
};
// Key order differs between the two JSON writers, so compare canonical forms.
const canonical = (v) => (v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canonical(v[k])])) : v);
const mismatches = Object.keys(expected).filter((key) => JSON.stringify(canonical(report[key])) !== JSON.stringify(canonical(expected[key])));
if (mismatches.length > 0) {
    for (const key of mismatches) console.log(`${key}: expected ${JSON.stringify(expected[key])}, got ${JSON.stringify(report[key])}`);
    process.exit(1);
}
console.log('BrowserPlatform behaves as the platform contract promises (timers, fetch, abort, timeout).');
