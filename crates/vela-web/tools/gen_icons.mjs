// Generates crates/vela-web/src/icons.rs from the original's icon registry
// (packages/vela-ui/src/lib/icons.ts). Only the ids the drawings chrome uses are emitted.
//
// usage: node gen_icons.mjs <path to icons.ts> <output .rs>

import { readFileSync, writeFileSync } from 'node:fs';

const [src, out] = process.argv.slice(2);
const text = readFileSync(src, 'utf8');

const WANT = [
    'cursor',
    'ruler',
    'magnet',
    'eraser',
    'pen-lock',
    'chevron-right',
    'chevrons-left',
    'chevrons-right',
    'check',
    'star',
    'star-filled',
    'lock',
    'unlock',
    'trash',
    'clone',
    'bring-front',
    'send-back',
    'close',
];

const registry = new Map();
const registerIcon = (id, svg) => registry.set(id, svg);
const svg16 = (body, extra = '') =>
    `<svg viewBox="0 0 16 16" width="1em" height="1em" ${extra ? extra + ' ' : ''}fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
const svg24 = (body, extra = '') =>
    `<svg viewBox="0 0 24 24" width="1em" height="1em" ${extra ? extra + ' ' : ''}fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
const svg24Solid = (body) => `<svg viewBox="0 0 24 24" width="1em" height="1em" fill="currentColor" stroke="none">${body}</svg>`;

// Execute only the registration section of the file (from the first `const S = svg16;`
// to the `iconEl` helper), with the helpers above in scope.
const start = text.indexOf('const S = svg16;');
const end = text.indexOf('/** A ready-to-insert element');
if (start < 0 || end < 0) throw new Error('icons.ts layout changed');
const body = text.slice(start, end);
new Function('registerIcon', 'svg16', 'svg24', 'svg24Solid', `${body}`)(registerIcon, svg16, svg24, svg24Solid);

let rs = `//! The icons the drawings chrome draws (toolbar, floating bar), copied from the original's
//! icon registry (\`packages/vela-ui/src/lib/icons.ts\`) by a script, not by hand. Every icon
//! strokes or fills \`currentColor\` and is sized \`1em\`.

/// The markup of icon \`id\`, or an empty string for an unknown id.
pub fn icon(id: &str) -> &'static str {
    match id {
`;
for (const id of WANT) {
    const svg = registry.get(id);
    if (!svg) throw new Error(`icon ${id} not found`);
    if (svg.includes('"####')) throw new Error('raw string delimiter clash');
    rs += `        ${JSON.stringify(id)} => r####"${svg}"####,\n`;
}
rs += `        _ => "",\n    }\n}\n`;
writeFileSync(out, rs);
console.log(`wrote ${WANT.length} icons to ${out}`);
