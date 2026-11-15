//! The stylesheet of the top shell: the original components' CSS (`topbar.ts`,
//! `symbol-picker.ts`, `indicator-picker.ts`, `layout-picker.ts`, `timeframe-quick.ts`,
//! `timeframe-drawer.ts`, `timezone-drawer.ts`, `bottombar.ts`, `statusline.ts`,
//! `watermark.ts`, `cell-controls.ts`) with the `${t.*}` design tokens written as
//! `var(--vela-*)`, plus the rules of the small kit pieces they stand on (dialog, dropdown
//! menu, tooltip, tabs, drawer: the original's `vela-ui` components, not ported as such).
//! One `<style id="vela-shell-top">` per document, in the `vela.chrome` layer.

use web_sys::Document;

use crate::dom::create_element;

pub const STYLE_ID: &str = "vela-shell-top";

pub const CSS: &str = r#"@layer vela.chrome {

/* ── kit: dialog ── */
.vela-st-backdrop { position: fixed; inset: 0; background: transparent; z-index: var(--vela-z-dialog); }
.vela-st-backdrop[data-dim] { background: var(--vela-backdrop); }
.vela-st-positioner { position: fixed; inset: 0; display: flex; align-items: flex-start; justify-content: center; padding-top: 10vh; z-index: var(--vela-z-dialog); pointer-events: none; }
[data-layout='mobile'] .vela-st-backdrop { position: absolute; }
[data-layout='mobile'] .vela-st-positioner { position: absolute; padding: 0; align-items: stretch; }
.vela-st-panel {
    pointer-events: auto;
    background: var(--vela-surface);
    color: var(--vela-fg);
    border: 1px solid var(--vela-border-strong);
    border-radius: var(--vela-radius-lg);
    box-shadow: var(--vela-shadow-dialog);
    font-size: 13px;
    font-family: var(--vela-font);
    min-width: 300px;
    max-width: min(92vw, 560px);
    max-height: 70vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    outline: 2px solid transparent;
    animation: vela-st-dialog-in var(--vela-dur-med) var(--vela-ease);
}
[data-layout='mobile'] .vela-st-panel { width: 100%; min-width: 0; max-width: none; max-height: none; border-radius: 0; border: none; }
@keyframes vela-st-dialog-in { from { opacity: 0; transform: translateY(-6px) scale(0.985); } to { opacity: 1; transform: none; } }
.vela-st-header { display: flex; align-items: center; padding-block: 9px; padding-inline: 16px 9px; border-bottom: 1px solid var(--vela-border); user-select: none; }
.vela-st-title { flex: 1; font-size: 17px; font-weight: 600; letter-spacing: 0.2px; color: var(--vela-fg-bright); }
.vela-st-close {
    all: unset; cursor: pointer; width: 30px; height: 30px; display: flex; align-items: center; justify-content: center;
    border-radius: var(--vela-radius-sm); color: var(--vela-fg-muted); line-height: 1; font-size: 15px;
}
.vela-st-close:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
[data-layout='mobile'] .vela-st-close { width: 40px; height: 40px; }
.vela-st-body { padding: var(--vela-space-4); overflow: auto; min-height: 0; flex: 1 1 auto; }
.vela-st-body[data-flush] { padding: 0; overflow: hidden; display: flex; flex-direction: column; }
[data-layout='mobile'] .vela-st-body { padding-bottom: calc(var(--vela-space-4) + env(safe-area-inset-bottom, 0px)); }
.vela-st-body[data-flush] { padding-bottom: 0; }

/* ── kit: dropdown menu, tooltip, tabs, drawer ── */
.vela-st-menu {
    position: absolute; z-index: var(--vela-z-menu); list-style: none; margin: 0;
    background: var(--vela-surface); color: var(--vela-fg); border: 1px solid var(--vela-border-strong);
    border-radius: var(--vela-radius-md); box-shadow: var(--vela-shadow); padding: 6px; min-width: 180px;
    max-height: 60vh; overflow-y: auto; font-size: 13px; font-family: var(--vela-font); outline: 2px solid transparent;
    animation: vela-st-menu-in var(--vela-dur-fast) var(--vela-ease);
}
@keyframes vela-st-menu-in { from { opacity: 0; transform: translateY(-3px); } to { opacity: 1; transform: translateY(0); } }
.vela-st-menu-item {
    all: unset; box-sizing: border-box; display: flex; align-items: center; gap: 10px; width: 100%; padding: 7px 10px;
    border-radius: var(--vela-radius-sm); cursor: pointer; user-select: none; white-space: nowrap;
}
.vela-st-menu-item[data-highlighted] { background: var(--vela-hover); }
.vela-st-menu-item[data-disabled] { opacity: 0.4; cursor: default; }
.vela-st-menu-item[data-checked] { background: var(--vela-hover-strong); color: var(--vela-fg-bright); }
.vela-st-menu-icon { width: 16px; height: 16px; font-size: 16px; display: inline-flex; justify-content: center; color: var(--vela-fg-muted); flex: none; }
.vela-st-menu-label { flex: 1 1 auto; }
.vela-st-menu-hint { color: var(--vela-fg-faint); font-size: var(--vela-font-size-sm); }
.vela-st-menu-badge { width: 24px; height: 24px; padding: 3px; flex: none; box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; border-radius: var(--vela-radius-md); background: var(--vela-hover); border: 1px solid var(--vela-border); color: var(--vela-fg-muted); }
.vela-st-menu-badge:empty { background: transparent; border-color: transparent; }
.vela-st-menu[data-badges] > .vela-st-menu-item { padding-block: 5px; padding-inline: 6px 10px; }
.vela-st-menu-item[data-checked] .vela-st-menu-badge:not(:empty) { background: color-mix(in srgb, var(--vela-fg-bright) 83%, var(--vela-surface)); border-color: color-mix(in srgb, var(--vela-fg-bright) 40%, transparent); color: var(--vela-surface); }
.vela-st-menu-star {
    order: 98; width: 26px; height: 22px; margin-block: -3px; margin-inline: 0 -5px; padding: 3px 5px; border-radius: var(--vela-radius-sm);
    opacity: 0; display: inline-flex; align-items: center; justify-content: center; box-sizing: border-box; flex: none; color: var(--vela-fg-muted);
}
.vela-st-menu-item[data-highlighted] .vela-st-menu-star { opacity: 0.55; }
.vela-st-menu-star:hover { opacity: 1; background: var(--vela-hover-strong); }
.vela-st-menu-star[data-fav] { opacity: 0.95; color: var(--vela-highlight); }
.vela-st-menu-sep { height: 1px; margin: 4px 6px; background: var(--vela-border); }
.vela-st-tip {
    position: absolute; z-index: var(--vela-z-tooltip); pointer-events: none; padding: 4px 8px; border-radius: var(--vela-radius-sm);
    background: var(--vela-surface-elev); color: var(--vela-fg); border: 1px solid var(--vela-border-strong); box-shadow: var(--vela-shadow);
    font-size: var(--vela-font-size-md); font-family: var(--vela-font); white-space: nowrap;
}
.vela-st-tabs { display: flex; gap: 14px; }
.vela-st-tab { all: unset; box-sizing: border-box; cursor: pointer; padding: 5px 10px; border-radius: var(--vela-radius-sm); color: var(--vela-fg-muted); font-weight: 600; white-space: nowrap; }
.vela-st-tab:hover { color: var(--vela-fg-bright); }
.vela-st-tab[aria-selected='true'] { color: var(--vela-fg-bright); background: var(--vela-hover-strong); }
.vela-st-drawer-backdrop { position: absolute; inset: 0; background: var(--vela-backdrop); z-index: 45; }
.vela-st-drawer {
    position: absolute; inset-inline: 0; bottom: 0; z-index: 46; max-height: 85%; display: flex; flex-direction: column;
    background: var(--vela-surface); color: var(--vela-fg); border-top: 1px solid var(--vela-border-strong);
    border-radius: 14px 14px 0 0; box-shadow: var(--vela-shadow-dialog); font-family: var(--vela-font);
    padding-bottom: env(safe-area-inset-bottom, 0px);
}
.vela-st-drawer-handle { flex: none; width: 36px; height: 4px; border-radius: 2px; background: var(--vela-border-strong); margin: 8px auto 4px; }
.vela-st-drawer-title { flex: none; padding: 4px 16px 8px; font-size: 15px; font-weight: 600; color: var(--vela-fg-bright); }
.vela-st-drawer-body { padding: 4px 16px 16px; overflow-y: auto; min-height: 0; touch-action: pan-y; }

/* ── topbar ── */
.vela-widget-topbar [data-topbar-hidden] { display: none !important; }
.vela-widget-topbar {
    display: flex; align-items: center; gap: var(--vela-space-1); padding: var(--vela-space-1) var(--vela-space-2);
    border-bottom: 1px solid var(--vela-border-soft); color: var(--vela-fg); font-size: var(--vela-font-size-md); flex: none;
}
[data-layout='mobile'] .vela-widget-topbar { display: none; }
.vela-widget-symbol, .vela-widget-tf, .vela-widget-style, .vela-widget-indicators, .vela-widget-action-left {
    all: unset; box-sizing: border-box; display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 9px;
    border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg-muted); font-size: 13px; font-weight: 550; white-space: nowrap;
}
.vela-widget-symbol { color: var(--vela-fg-bright); font-size: 15px; font-weight: 600; letter-spacing: 0.3px; padding: 0 10px; gap: 7px; }
.vela-widget-tf, .vela-widget-style, .vela-widget-indicators, .vela-widget-action-left { color: var(--vela-fg-bright); }
.vela-widget-symbol:hover, .vela-widget-tf:hover, .vela-widget-style:hover, .vela-widget-indicators:hover, .vela-widget-action-left:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-widget-tf-group { display: inline-flex; align-items: center; gap: 2px; }
.vela-widget-tf-chips { display: inline-flex; align-items: center; gap: 2px; }
.vela-widget-tf-chips:empty { display: none; }
.vela-widget-tf[data-current='1'] { background: var(--vela-hover-strong); color: var(--vela-fg-bright); }
.vela-widget-tf-caret {
    all: unset; box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 30px;
    border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg-muted);
}
.vela-widget-tf-caret:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-widget-tf-caret[data-solo='1'] { width: auto; padding: 0 6px 0 9px; gap: 4px; color: var(--vela-fg-bright); font-size: 13px; font-weight: 550; white-space: nowrap; }
.vela-widget-topbar .vela-widget-tf-caret .vela-icon { font-size: 14px; width: 14px; height: 14px; }
.vela-icon { display: inline-flex; align-items: center; justify-content: center; line-height: 0; }
.vela-icon svg { display: block; }
.vela-widget-topbar .vela-icon { color: inherit; font-size: 16px; width: 16px; height: 16px; }
.vela-sep { height: 22px; margin: 0 2px; flex: none; background: var(--vela-border-strong); }
.vela-alerts-badge {
    position: absolute; top: 2px; inset-inline-end: 2px; min-width: 13px; height: 13px; padding: 0 3px; border-radius: 7px;
    background: var(--vela-accent); color: var(--vela-fg-on-fill); font-size: 9px; font-weight: 700; display: inline-flex; align-items: center; justify-content: center;
}
.vela-topbar-right { margin-inline-start: auto; display: inline-flex; align-items: center; gap: var(--vela-space-1); }
.vela-widget-actions { display: inline-flex; gap: var(--vela-space-1); }
.vela-widget-actions-left { display: inline-flex; align-items: center; gap: var(--vela-space-1); }
.vela-widget-action-pin { display: inline-flex; align-items: center; }
.vela-widget-panels { display: inline-flex; align-items: center; gap: var(--vela-space-1); }
.vela-widget-tool {
    all: unset; box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; width: 32px; height: 30px;
    border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg-muted); font-size: 14px;
}
.vela-widget-tool:hover:not(:disabled) { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-widget-tool:disabled { opacity: 0.35; cursor: default; }
.vela-widget-tool[data-active='1'] { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-widget-action { all: unset; box-sizing: border-box; display: inline-flex; align-items: center; gap: 6px; padding: 4px 8px; border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg); }
.vela-widget-action:hover { background: var(--vela-hover); }

/* ── symbol picker ── */
.vela-sp-head { flex: none; padding: var(--vela-space-4) var(--vela-space-4) 0; }
.vela-sp-searchrow { display: flex; align-items: center; gap: 8px; height: 40px; padding: 0 12px; background: var(--vela-surface-elev); border: 1px solid var(--vela-border); border-radius: 8px; }
.vela-sp-searchrow:focus-within { border-color: var(--vela-border-strong); }
.vela-sp-searchrow .vela-icon { color: var(--vela-fg-muted); font-size: 16px; }
.vela-sp-input { flex: 1; background: transparent; color: var(--vela-fg); border: none; font-size: 14px; outline: none; text-transform: uppercase; }
.vela-sp-input::placeholder { text-transform: none; }
.vela-sp-tabs { margin: 12px 2px 0; border-bottom: 1px solid var(--vela-border); padding-bottom: 8px; }
[data-layout='mobile'] .vela-sp-tabs { overflow-x: auto; scrollbar-width: none; gap: 6px; }
[data-layout='mobile'] .vela-sp-tabs::-webkit-scrollbar { display: none; }
[data-layout='mobile'] .vela-sp-tabs > .vela-st-tab { flex: none; padding: 7px 10px; }
[data-layout='mobile'] .vela-sp-list { flex: 1 1 auto; max-height: none; margin-bottom: 0; }
.vela-sp-list { min-height: 0; margin: var(--vela-space-2) var(--vela-space-4) var(--vela-space-4); max-height: 46vh; overflow: auto; }
.vela-sp-row { display: flex; align-items: center; gap: 12px; padding: 7px 10px; border-radius: 8px; cursor: pointer; }
.vela-sp-row:hover, .vela-sp-row[data-highlighted] { background: var(--vela-hover); }
.vela-sp-avatar, .vela-sl-avatar { width: 28px; height: 28px; border-radius: 50%; flex: none; display: inline-flex; align-items: center; justify-content: center; color: var(--vela-fg-on-fill); font-size: var(--vela-font-size-md); font-weight: 700; overflow: hidden; }
.vela-sp-avatar img, .vela-sl-avatar img { width: 100%; height: 100%; object-fit: cover; display: block; }
.vela-sp-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.vela-sp-ticker { font-weight: 700; color: var(--vela-fg-bright); font-size: 14px; text-transform: uppercase; }
.vela-sp-desc { color: var(--vela-fg-muted); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-sp-badge { flex: none; padding: 2px 8px; border-radius: var(--vela-radius-sm); background: var(--vela-surface-elev); border: 1px solid var(--vela-border); color: var(--vela-fg-muted); font-size: 10px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }
.vela-sp-badge[data-p='binance'] { color: #f0b90b; }
.vela-sp-badge[data-p='hyperliquid'] { color: #50d2c1; }
.vela-sp-empty { padding: var(--vela-space-3); color: var(--vela-fg-muted); text-align: center; }
.vela-sp-expander { all: unset; box-sizing: border-box; flex: none; display: inline-flex; align-items: center; justify-content: center; width: 22px; height: 22px; border-radius: 5px; cursor: pointer; color: var(--vela-fg-muted); }
.vela-sp-expander:hover { background: var(--vela-surface-elev); color: var(--vela-fg); }
.vela-sp-row[data-member] { padding-inline-start: 34px; }

/* ── indicator picker ── */
.vela-ip-searchrow { display: flex; align-items: center; gap: 8px; height: 40px; padding: 0 12px; margin-bottom: var(--vela-space-2); background: var(--vela-surface-elev); border: 1px solid var(--vela-border); border-radius: 8px; }
.vela-ip-searchrow:focus-within { border-color: var(--vela-border-strong); }
.vela-ip-searchrow .vela-icon { color: var(--vela-fg-muted); font-size: 16px; }
.vela-ip-search { flex: 1; background: transparent; color: var(--vela-fg); border: none; font-size: 14px; outline: none; }
.vela-ip-list { max-height: 50vh; overflow: auto; min-width: 380px; }
[data-layout='mobile'] .vela-ip-list { min-width: 0; max-height: none; }
.vela-ip-group { color: var(--vela-fg-muted); font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.08em; padding: 10px 4px 4px; border-bottom: 1px solid var(--vela-border); margin-bottom: 2px; }
.vela-ip-oncard { background: var(--vela-surface-elev); border: 1px solid var(--vela-border); border-radius: 8px; padding: 6px 8px; margin-bottom: var(--vela-space-2); }
.vela-ip-oncard .vela-ip-group { border-bottom: none; padding-top: 2px; }
.vela-ip-row { display: flex; align-items: center; gap: var(--vela-space-2); padding: 7px 6px; border-radius: var(--vela-radius-sm); cursor: pointer; }
.vela-ip-row:hover { background: var(--vela-hover); }
.vela-ip-name { flex: 1; font-weight: 600; color: var(--vela-fg-bright); font-size: 13px; }
.vela-ip-badge { flex: none; padding: 1px 7px; border-radius: var(--vela-radius-sm); background: var(--vela-surface-elev); border: 1px solid var(--vela-border); color: var(--vela-accent); font-size: 9px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }
.vela-ip-trash { all: unset; box-sizing: border-box; cursor: pointer; color: var(--vela-fg-muted); padding: 2px 4px; border-radius: var(--vela-radius-sm); font-size: var(--vela-font-size-md); display: inline-flex; }
.vela-ip-trash:hover { color: var(--vela-down); background: var(--vela-hover); }
.vela-ip-empty { padding: var(--vela-space-3); color: var(--vela-fg-muted); text-align: center; }

/* ── layout picker ── */
.vela-lp-layer { position: absolute; z-index: var(--vela-z-menu); }
.vela-lp {
    background: var(--vela-surface); color: var(--vela-fg); border: 1px solid var(--vela-border-strong); border-radius: 8px; box-shadow: var(--vela-shadow);
    padding: 10px 12px 10px; font-size: 13px; user-select: none; transform-origin: top; animation: vela-lp-in var(--vela-dur-fast) var(--vela-ease);
}
@keyframes vela-lp-in { from { opacity: 0; transform: translateY(-4px) scale(0.98); } to { opacity: 1; transform: translateY(0) scale(1); } }
.vela-lp-cols { display: flex; align-items: stretch; gap: 14px; }
.vela-lp-heading { display: flex; align-items: center; gap: 8px; font-size: 10px; font-weight: 600; letter-spacing: 1.2px; text-transform: uppercase; color: var(--vela-fg-faint); margin-bottom: 10px; }
.vela-lp-badge { display: inline-flex; align-items: center; justify-content: center; width: 14px; height: 14px; border-radius: 50%; border: 1px solid var(--vela-border-strong); color: var(--vela-fg-muted); font-size: 9px; font-weight: 600; letter-spacing: 0; cursor: default; }
.vela-lp-badge:hover { color: var(--vela-fg-bright); border-color: var(--vela-fg-muted); }
.vela-lp-tip { display: flex; flex-direction: column; gap: 4px; max-width: 230px; white-space: normal; }
.vela-lp-vsep { width: 1px; flex: none; align-self: stretch; background: var(--vela-border-faint); }
.vela-lp-layout { width: 132px; display: flex; flex-direction: column; align-items: center; }
.vela-lp-layout > .vela-lp-heading, .vela-lp-layout > .vela-lp-presets { align-self: stretch; }
.vela-lp-grid { display: grid; grid-template-columns: repeat(4, 24px); grid-auto-rows: 24px; gap: 4px; }
.vela-lp-sq { all: unset; box-sizing: border-box; border-radius: var(--vela-radius-sm); background: var(--vela-hover); border: 1px solid var(--vela-border-faint); cursor: pointer; }
.vela-lp-sq:hover { background: var(--vela-hover-strong); border-color: var(--vela-border-strong); }
.vela-lp-sq[data-on='1'] { background: var(--vela-selected-bg); border-color: var(--vela-selected-bg); }
.vela-lp-presets { display: flex; flex-direction: column; gap: 2px; margin-top: 8px; }
.vela-lp-preset { all: unset; box-sizing: border-box; padding: 5px 8px; border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg-muted); font-size: 12px; white-space: nowrap; }
.vela-lp-preset:hover { background: var(--vela-hover); }
.vela-lp-preset[data-checked='1'] { background: var(--vela-hover-strong); color: var(--vela-fg-bright); }
.vela-lp-sync { display: flex; flex-direction: column; gap: 4px; min-width: 118px; }
.vela-lp-sync-row { all: unset; box-sizing: border-box; display: flex; align-items: center; gap: 14px; padding: 6px 8px; border-radius: 5px; cursor: pointer; }
.vela-lp-sync-row:hover { background: var(--vela-hover); }
.vela-lp-sync-row .vela-lp-label { flex: 1 1 auto; }
.vela-lp-switch { position: relative; flex: none; width: 34px; height: 18px; border-radius: 9px; background: var(--vela-hover); border: 1px solid var(--vela-border-soft); }
.vela-lp-switch::after { content: ''; position: absolute; top: 2px; inset-inline-start: 2px; width: 12px; height: 12px; border-radius: 50%; background: var(--vela-fg-muted); transition: transform 0.16s ease, background 0.16s ease; }
.vela-lp-switch.on { background: var(--vela-selected-bg); border-color: var(--vela-selected-bg); }
.vela-lp-switch.on::after { transform: translateX(16px); background: var(--vela-selected-fg); }

/* ── timeframe quick entry ── */
.vela-tq-input { display: block; margin: 0 auto; width: 220px; box-sizing: border-box; height: 40px; background: var(--vela-surface-elev); color: var(--vela-fg); border: 1px solid var(--vela-border); border-radius: 8px; padding: 0 12px; font-size: 18px; text-align: center; outline: none; }
.vela-tq-input:focus { border-color: var(--vela-border-strong); }
.vela-tq-hint { margin-top: var(--vela-space-2); text-align: center; color: var(--vela-fg-muted); min-height: 1.2em; }
.vela-tq-hint[data-invalid] { color: var(--vela-danger); }

/* ── timeframe and timezone drawers ── */
.vela-tfd-heading { padding: 4px 2px 8px; font-size: 12px; font-weight: 600; letter-spacing: 0.4px; text-transform: uppercase; color: var(--vela-fg-bright); }
.vela-tfd-ranges { display: flex; gap: 6px; overflow-x: auto; scrollbar-width: none; touch-action: pan-x; padding: 0 2px 14px; }
.vela-tfd-ranges::-webkit-scrollbar { display: none; }
.vela-tfd-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(64px, 1fr)); gap: 6px; padding-bottom: 4px; }
.vela-tfd-chip { all: unset; box-sizing: border-box; min-height: 40px; padding: 0 14px; display: inline-flex; align-items: center; justify-content: center; flex: none; border: 1px solid var(--vela-border); border-radius: 8px; font-size: 13px; font-weight: 600; color: var(--vela-fg); cursor: pointer; -webkit-tap-highlight-color: transparent; }
.vela-tfd-chip:active { background: var(--vela-hover); }
.vela-tfd-chip[data-active='1'] { color: var(--vela-fg-bright); border-color: var(--vela-fg-bright); background: var(--vela-hover); }
.vela-tzd-list { padding: 2px 0 4px; }
.vela-tzd-row { display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 0 2px; border-radius: 8px; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.vela-tzd-row:active { background: var(--vela-hover); }
.vela-tzd-row-label { flex: 1 1 auto; min-width: 0; font-size: 14px; color: var(--vela-fg-bright); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-tzd-row .vela-icon { flex: none; color: var(--vela-fg-bright); font-size: 16px; }

/* ── bottom bar ── */
.vela-widget-bottombar { display: flex; align-items: center; gap: 2px; height: 38px; padding: 0 8px; border-top: 1px solid var(--vela-border); color: var(--vela-fg-muted); font-size: 12px; flex: none; }
[data-layout='mobile'] .vela-widget-bottombar { display: none; }
.vela-bb-range { all: unset; box-sizing: border-box; height: 24px; display: inline-flex; align-items: center; padding: 0 9px; border-radius: var(--vela-radius-sm); font-weight: 600; cursor: pointer; }
.vela-bb-range:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-bb-range[data-active='1'] { color: var(--vela-fg-bright); background: var(--vela-hover); }
.vela-bb-spacer { flex: 1 1 auto; }
.vela-bb-clock { font-variant-numeric: tabular-nums; color: var(--vela-fg-bright); font-weight: 600; }
.vela-bb-tz { all: unset; box-sizing: border-box; height: 26px; display: inline-flex; align-items: center; gap: 8px; padding: 0 8px; border-radius: var(--vela-radius-sm); font-weight: 600; color: var(--vela-fg-bright); cursor: pointer; }
.vela-bb-tz:hover { background: var(--vela-hover); }
.vela-bb-session { display: inline-flex; border: 1px solid var(--vela-border-strong); border-radius: var(--vela-radius-sm); overflow: hidden; margin-inline-start: 6px; }
.vela-bb-session-btn { all: unset; box-sizing: border-box; height: 24px; display: inline-flex; align-items: center; padding: 0 8px; color: var(--vela-fg-muted); font-size: 11px; font-weight: 600; cursor: pointer; }
.vela-bb-session-btn:disabled { cursor: not-allowed; opacity: 0.55; }
.vela-bb-session-btn:not(:disabled):hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-bb-session-btn.is-active { color: var(--vela-fg); background: var(--vela-surface-elev); }
.vela-bb-session-btn.is-active:disabled { opacity: 0.8; }
.vela-bb-settings { all: unset; box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; width: 28px; height: 26px; margin-inline-start: 6px; border-radius: var(--vela-radius-sm); cursor: pointer; color: var(--vela-fg-muted); font-size: 14px; }
.vela-bb-settings:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }

/* ── status line ── */
.vela-statusline {
    position: absolute; top: var(--vela-space-2); inset-inline-start: calc(var(--vela-toolbar-gutter, 0px) + 10px); z-index: 10;
    display: flex; align-items: baseline; gap: var(--vela-space-2);
    max-width: calc(100% - var(--vela-toolbar-gutter, 0px) - var(--vela-scale-gutter, 0px) - 24px); overflow: hidden;
    color: var(--vela-fg); font-size: var(--vela-font-size-md); pointer-events: auto;
    background: color-mix(in srgb, var(--vela-bg) 60%, transparent); border-radius: var(--vela-radius-sm); padding: 2px 7px; margin-inline-start: -7px;
}
.vela-statusline:hover { background: var(--vela-bg); box-shadow: inset 0 0 0 1px var(--vela-border); }
.vela-statusline.vela-sl-chart-hidden { opacity: 0.5; }
.vela-statusline .vela-sl-row { display: flex; align-items: baseline; gap: var(--vela-space-2); white-space: nowrap; }
.vela-statusline.vela-sl-stacked { flex-direction: column; align-items: flex-start; row-gap: var(--vela-space-1); }
.vela-statusline.vela-sl-stacked .vela-sl-identity { max-width: 100%; }
.vela-statusline.vela-sl-stacked .vela-sl-meta { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.vela-statusline .vela-sl-avatar { width: 18px; height: 18px; font-size: 10px; align-self: center; }
.vela-statusline .vela-sl-symbol { font-weight: 600; font-size: var(--vela-font-size-lg); }
.vela-statusline .vela-sl-meta { color: var(--vela-fg-muted); font-size: var(--vela-font-size-md); font-weight: 600; }
.vela-statusline .vela-sl-symbol + .vela-sl-meta { margin-inline-start: calc(var(--vela-space-1) - var(--vela-space-2)); }
.vela-statusline .vela-sl-market { align-self: center; display: inline-flex; }
.vela-statusline .vela-sl-market > [hidden] { display: none !important; }
.vela-sl-bubble { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: 50%; font-size: 10px; line-height: 0; }
.vela-statusline .vela-sl-replay-badge, .vela-statusline .vela-sl-replay-badge svg { display: block; width: 16px; height: 16px; }
.vela-statusline .vela-sl-values { font-variant-numeric: tabular-nums; }
.vela-statusline .vela-sl-ohlc { display: flex; gap: var(--vela-space-1); color: var(--vela-fg-muted); }
.vela-statusline .vela-sl-ohlc b { color: var(--vela-fg); font-weight: 500; }
.vela-statusline .vela-sl-ohlc b, .vela-statusline .vela-sl-change { unicode-bidi: isolate; }
.vela-statusline .vela-sl-change[data-dir='up'] { color: var(--vela-up); }
.vela-statusline .vela-sl-change[data-dir='down'] { color: var(--vela-down); }
.vela-statusline .vela-sl-eye { align-self: center; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; border: none; border-radius: 3px; background: none; color: var(--vela-fg-muted); cursor: pointer; line-height: 0; flex: none; }
.vela-statusline .vela-sl-eye:hover { color: var(--vela-fg); background: color-mix(in srgb, var(--vela-fg) 12%, transparent); }
[data-vela-has-statusline] [data-vela-pane-at-top] { transform: translateY(26px); }
[data-vela-has-statusline][data-vela-sl-stacked-host] [data-vela-pane-at-top] { transform: translateY(40px); }
[data-layout='mobile'] .vela-statusline { font-size: var(--vela-font-size-sm); }
[data-layout='mobile'] .vela-statusline .vela-sl-symbol { font-size: var(--vela-font-size-md); }

/* ── watermark ── */
.vela-watermark {
    position: absolute; top: var(--vela-price-pane-top, 0px); bottom: var(--vela-price-pane-bottom, 0px);
    inset-inline-start: var(--vela-toolbar-gutter, 0px); inset-inline-end: var(--vela-scale-gutter, 0px);
    display: flex; flex-direction: column; align-items: center; justify-content: center; overflow: hidden; pointer-events: none; z-index: 1;
    color: var(--vela-fg); opacity: 0.05; font-size: 36px; font-weight: 700; letter-spacing: 0.04em; user-select: none; white-space: nowrap;
}
.vela-watermark-replay { display: inline-flex; align-items: center; gap: 0.25em; margin-top: 0.1em; font-size: 0.8em; }
.vela-watermark [hidden] { display: none !important; }

/* ── cell controls ── */
.vela-cc-btn { display: inline-flex; align-items: center; justify-content: center; width: 20px; height: 20px; padding: 0; border: none; border-radius: var(--vela-radius-sm); background: transparent; line-height: 0; font-size: 12px; color: var(--vela-fg-muted); cursor: pointer; }
.vela-cc-btn svg { display: block; }
.vela-cc-btn:hover { background: var(--vela-active); color: var(--vela-fg-bright); }
.vela-cc-on, .vela-cc-on:hover { background: var(--vela-selected-bg); color: var(--vela-selected-fg); }
.vela-cc-grip { cursor: grab; touch-action: none; }
.vela-cc-grip:active { cursor: grabbing; }

}"#;

/// Inject the stylesheet once per document.
pub fn ensure_styles(doc: &Document) {
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let style = create_element(doc, "style");
    style.set_id(STYLE_ID);
    style.set_text_content(Some(CSS));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    } else if let Some(root) = doc.document_element() {
        let _ = root.append_child(&style);
    }
}
