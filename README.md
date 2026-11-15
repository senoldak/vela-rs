# vela-rs

An unofficial Rust port of [LuxAlgo/Vela](https://github.com/LuxAlgo/Vela), the financial charting library, compiled to WebAssembly for the browser.

This project is not affiliated with LuxAlgo. It tracks upstream commit [`fc3e2a0`](https://github.com/LuxAlgo/Vela/commit/fc3e2a011834452db766bc154ef0549a1583c66b).

## Features

- Candlestick, bar, line, area, baseline, column and Heikin Ashi charts
- 75 built-in indicators and 79 drawing tools
- Canvas 2D and WebGL2 rendering backends
- Multi-chart workspaces with layouts, splitters and cross-chart sync
- A full widget shell: top bar, symbol and indicator pickers, sidebar, object tree, data window, status line
- Data providers for Binance, Coinbase and Hyperliquid, plus custom providers written in JavaScript
- 20 locale packs, light and dark themes, custom themes
- Persistent state for charts, drawings and workspaces

## Crates

| Crate | Purpose |
|---|---|
| `vela-core` | Platform-neutral core: model, indicators, drawings, data layer, engine, renderer, workspace and widget logic. Builds natively and for `wasm32-unknown-unknown`. Depends only on `serde` and `serde_json`. |
| `vela-web` | Browser layer over `wasm-bindgen`: DOM host, input handling, UI, and the JavaScript classes. |
| `xtask` | Packages `vela-web` into a WebAssembly bundle. |

`vela-core` never touches the DOM. It reaches the platform through a small set of traits (`Platform`, `Intl`, `Canvas2d`, `Gl`, `NativeHost`, `WorkspaceHost`), which `vela-web` implements for the browser.

## Getting started

Requires Rust 1.85 or later and the `wasm32-unknown-unknown` target.

```bash
rustup target add wasm32-unknown-unknown
```

Build the browser package into `pkg/`:

```bash
cargo run -p xtask -- web pkg
```

Serve the repository root and open the demos:

```bash
python -m http.server 8000
```

| Page | Shows |
|---|---|
| `demo/index.html` | A single chart with drawings, legend and settings |
| `demo/workspace.html` | A 2×2 multi-chart workspace |
| `demo/widget.html` | The full widget shell (`?lang=tr` switches the locale) |
| `demo/api.html` | Embedding through the `Vela` class |

All demos run on synthetic data and make no network requests.

## Usage

### JavaScript

The `Vela` class mirrors the original public API: the same constructor, control surfaces (`data`, `panes`, `replay`, `renderer`, `drawings`, `marks`) and events.

```js
import init, { Vela } from './pkg/vela_web.js';

await init();

const chart = new Vela('#chart', { data: bars, timeframe: '1h', theme: 'dark' });
const rsi = chart.addNativeIndicator('rsi');

await chart.ready();
chart.on('market:changed', ({ symbol, timeframe }) => console.log(symbol, timeframe));
```

`VelaWorkspace` and `VelaWidget` provide the multi-chart workspace and the widget shell. Extension points are registered page-wide: `registerLocale`, `registerTheme`, `registerWidgetAction`, `registerSidePanel`, `registerLegendAction`, `registerSymbolRanking` and others.

### Rust

```rust
use vela_core::indicators::classics::{classic_native_output, classic_specs};
use vela_core::model::Ohlcv;

let bars: Vec<Ohlcv> = (0..100)
    .map(|i| {
        let close = 100.0 + i as f64;
        Ohlcv::new(i * 60_000, close, close + 1.0, close - 1.0, close, Some(1_000.0))
    })
    .collect();

let specs = classic_specs();
let rsi = specs.iter().find(|s| s.indicator_type == "rsi").unwrap();

// Value series aligned with the bars; NaN during warm-up.
let out = rsi.compute(&bars, &rsi.default_inputs());

// Render-ready output: stable series ids, fills, level lines.
let scene = classic_native_output(rsi, &bars, &rsi.default_inputs());
```

## Not ported

- **Script engine.** The original runs Pine scripts through a separate engine, which this port does not include. `addIndicator(source)`, `runScript` and `registerEngine` fail with a clear error. Built-in indicators work.
- **UI kit.** The original component kit (`packages/vela-ui`) is not ported. The UI reproduces its structure and behavior with native HTML elements: a color input with an opacity slider in place of the color picker, `<select>` for dropdowns, native date and time inputs. Exit animations and swipe-to-dismiss are absent.
- **Some JavaScript exports.** `registerChartType`, `registerNativeIndicator`, `registerRendererLayer`, `createTheme` and the exported classes (`NativeRenderer`, `DrawingStore`, `MultiProviderFeed`) exist in Rust but are not exposed to JavaScript.
- **Function-valued options.** `Vela` rejects a custom renderer class, custom formatters, a price-axis tick function and lazy mark content with an explicit error.

## Verification

Run the test suite and lints:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
```

### Differential checks against the original

Each check runs the same inputs through the Rust port and the original TypeScript, then compares the output. They need a checkout of the original and Node 23.6 or later; the scripts load the TypeScript sources directly, so no `npm install` is required.

```bash
cargo run --release --example dump_catalog > catalog.json
node tools/parity/compare.mjs <path-to-Vela> catalog.json
```

| Check | Dump example | Script | Coverage | Result |
|---|---|---|---|---|
| Indicators | `dump_catalog` | `compare.mjs` | 75 indicators, 840 runs, 2.35M values | Max relative error 9.4e-14 |
| Drawing tools | `dump_drawings` | `compare_drawings.mjs` | 79 tools, 1,366 scenarios | Identical |
| Pure helpers | `dump_misc` | `compare_misc.mjs` | 112 checks | Identical |
| Formatting and i18n | `xtask web pkg-parity --parity` | `compare_format.mjs` | 113,004 requests, 20 locales | Identical |
| Drawing painter | `dump_paint` | `compare_paint.mjs` | 3,546 scenarios, 237,783 canvas calls | Identical |
| Indicator drawings | `dump_scene` | `compare_scene.mjs` | 706 scenes, 41,536 canvas calls | Identical |
| Series and axes | `dump_series` | `compare_series.mjs` | 961 scenarios, 3.69M canvas calls | Identical |
| WebGL2 backend | `dump_gl` | `compare_gl.mjs` | 643 frames, 106,473 GL calls | Identical |

Painters are compared by their call streams (name, order and arguments), not by pixels.

### Browser smoke tests

Eleven scripts under `tools/parity/smoke_*.mjs` run the WebAssembly package in Node against a fake DOM and cover wiring and behavior: input, drawings, dialogs, the workspace, the widget shell, WebGL2 fallback and the `Vela` API.

```bash
node tools/parity/smoke_chart.mjs pkg
```

The fake DOM performs no layout or painting, so these tests do not cover appearance.

### Known gaps

- The data providers are tested against mocked responses only. Nothing has been run against a live exchange.
- The engine, workspace and widget logic are covered by tests ported from the original, not by differential runs.
- The demos were checked manually in one Chromium-based browser. Touch input, mobile layout and other browsers are untested, and rendering was not compared pixel for pixel with the original.

## Differences from the original

- Module-level registries (chart types, indicators, drawing tools, themes, locales) are explicit values carried in `VelaEnv`.
- The core is single-threaded and asynchronous through `runtime::Platform`. It preserves the original's event ordering by yielding wherever the original awaits.
- Timestamps are `i64` milliseconds. A bar without volume has `volume: None`.
- Saved properties are type-checked when read; the original stores some values unchecked.
- With a NaN visible range, the Canvas 2D backend clears the canvas and stops, and the volume profile layer returns where the original throws.
- After `destroy()`, calls on `Vela` are no-ops.

## License

Apache-2.0. The original `LICENSE` and `NOTICE` files are included. The attribution requirement in `NOTICE` applies to this port: a product that displays a chart rendered by Vela must visibly show the Vela name and project link.
