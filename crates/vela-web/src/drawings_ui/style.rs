//! The stylesheet of the drawings widgets (the original's `vela-drawing-toolbar-styles`,
//! re-expressed over the `--vela-chrome-*` custom properties the host writes instead of the
//! `vela-ui` kit tokens) and the small markup helpers the toolbar and the bar share.

use web_sys::Document;

use crate::dom::create_element;

const STYLE_ID: &str = "vela-drawings-ui-styles";

/// Rules for the toolbar, its flyouts, the floating bar and the menus. Hover and active
/// tints are CSS (`:hover`, `[data-active]`) so they never lag or stick. Layered like the
/// host's own rules, so a host stylesheet can override any of them.
const CSS: &str = r#"@layer vela.chrome {

.vela-dtb, .vela-dtb-flyout, .vela-dmenu {
    --dtb-fg: var(--vela-chrome-text-color, #b2b5be);
    --dtb-bg: var(--vela-chrome-background, #151619);
    --dtb-border: var(--vela-chrome-border-color, #2a2b30);
    --dtb-muted: color-mix(in srgb, var(--dtb-fg) 72%, transparent);
    --dtb-hover: color-mix(in srgb, var(--dtb-fg) 11%, transparent);
    --dtb-hover-strong: color-mix(in srgb, var(--dtb-fg) 16%, transparent);
    --dtb-active: color-mix(in srgb, var(--dtb-fg) 20%, transparent);
    --dtb-gold: #f5b50a;
    --dtb-radius: 6px;
    font-family: var(--vela-chrome-font-family, sans-serif);
    box-sizing: border-box;
}
.vela-dtb { background: var(--dtb-bg); border-right: 1px solid var(--dtb-border); color: var(--dtb-muted); }
.vela-dtb button, .vela-dmenu button { font: inherit; }
.vela-dmenu-note { padding: 6px 10px; opacity: 0.65; white-space: nowrap; }
.vela-dtb button:focus-visible, .vela-dmenu button:focus-visible,
.vela-dtb-star:focus-visible { outline: 2px solid color-mix(in srgb, var(--dtb-fg) 70%, transparent); outline-offset: -2px; }

.vela-dtb-hit { width: 22px; height: 22px; display: flex; align-items: center; justify-content: center; border-radius: var(--dtb-radius); transition: background .12s ease, color .12s ease; }
.vela-dtb-hit--arrow { width: 11px; height: 22px; }
.vela-dtb-btn { position: relative; width: 100%; height: 30px; flex: none; display: flex; align-items: center; justify-content: center; cursor: pointer; color: inherit; background: transparent; border: none; padding: 0; transition: color .12s ease; }
.vela-dtb-btn:hover { color: var(--dtb-fg); }
.vela-dtb-btn:hover .vela-dtb-hit { background: var(--dtb-hover); }
.vela-dtb-btn[data-active='1'] { color: var(--dtb-fg); }
.vela-dtb-btn[data-active='1'] .vela-dtb-hit { background: var(--dtb-active); }
.vela-dtb-cell { position: relative; display: flex; align-items: center; justify-content: center; width: 100%; height: 30px; flex: none; }
.vela-dtb-icon { flex: none; width: 26px; height: 30px; display: flex; align-items: center; justify-content: center; background: transparent; border: none; color: inherit; cursor: pointer; padding: 0; transition: color .12s ease; }
.vela-dtb-icon:hover { color: var(--dtb-fg); }
.vela-dtb-cell[data-active='1'] .vela-dtb-icon { color: var(--dtb-fg); }
.vela-dtb-cell[data-active='1'] .vela-dtb-icon .vela-dtb-hit { background: var(--dtb-active); }
.vela-dtb-arrow { position: absolute; inset-inline-end: 1px; top: 50%; transform: translateY(-50%); display: flex; align-items: center; justify-content: center; background: transparent; border: none; color: inherit; cursor: pointer; padding: 0; opacity: 0; pointer-events: none; transition: opacity .12s ease, color .12s ease; }
.vela-dtb-cell:hover .vela-dtb-arrow, .vela-dtb-icon:hover ~ .vela-dtb-arrow, .vela-dtb-cell.vela-open .vela-dtb-arrow, .vela-dtb-arrow:focus-visible { opacity: 1; pointer-events: auto; }
.vela-dtb-arrow:hover { color: var(--dtb-fg); }
.vela-dtb-icon:hover .vela-dtb-hit, .vela-dtb-arrow:hover .vela-dtb-hit { background: var(--dtb-hover); }
.vela-dtb-cell.vela-open .vela-dtb-arrow .vela-dtb-hit { background: var(--dtb-active); }
.vela-dtb-cell.vela-open .vela-dtb-arrow:hover .vela-dtb-hit { background: var(--dtb-hover); }
.vela-dtb-collapse { margin-top: auto; }
.vela-dtb[data-collapsed='1'] > *:not(.vela-dtb-collapse) { display: none; }
.vela-dtb[data-collapsed='1'] .vela-dtb-collapse .vela-dtb-hit { width: 14px; }

.vela-dtb-flyout { position: absolute; z-index: 23; display: flex; flex-direction: column; gap: 2px; padding: 4px; border-radius: 8px; background: var(--dtb-bg); border: 1px solid var(--dtb-border); box-shadow: 0 6px 20px rgba(0,0,0,.35); pointer-events: auto; overflow-y: auto; overscroll-behavior: contain; color: var(--dtb-fg); }
.vela-dtb-header { padding: 6px 12px 2px 8px; font-size: 11px; font-weight: 600; letter-spacing: .04em; color: var(--dtb-muted); user-select: none; text-transform: uppercase; }
.vela-dtb-sep { height: 1px; margin: 4px 8px; background: var(--dtb-border); }
.vela-dtb-item { display: flex; align-items: center; gap: 8px; padding: 5px 10px 5px 8px; cursor: pointer; color: var(--dtb-fg); border-radius: 4px; font-size: 13px; white-space: nowrap; min-width: 148px; background: transparent; border: none; text-align: start; transition: background .12s ease; }
.vela-dtb-item[data-badged='1'] { gap: 10px; padding: 4px 10px 4px 6px; }
.vela-dtb-item:hover { background: var(--dtb-hover-strong); }
.vela-dtb-label { flex: 1 1 auto; text-align: start; }
.vela-dtb-check { width: 14px; height: 14px; display: flex; align-items: center; justify-content: center; flex: none; }
.vela-dtb-hint { flex: none; font-size: 12px; color: var(--dtb-muted); }
.vela-dtb-badge { width: 24px; height: 24px; padding: 3px; box-sizing: border-box; display: flex; align-items: center; justify-content: center; flex: none; border-radius: var(--dtb-radius); background: var(--dtb-hover); border: 1px solid var(--dtb-border); color: var(--dtb-muted); transition: background .12s ease, color .12s ease; }
.vela-dtb-badge:empty { background: transparent; border-color: transparent; }
.vela-dtb-badge svg { width: 100%; height: 100%; }
.vela-dtb-item:hover .vela-dtb-badge:not(:empty) { background: var(--dtb-active); color: var(--dtb-fg); }
.vela-dtb-item[data-selected='1'] .vela-dtb-badge:not(:empty) { background: var(--dtb-fg); border-color: var(--dtb-fg); color: var(--dtb-bg); }
.vela-dtb-star { width: 26px; height: 22px; margin: -3px -5px -3px 0; padding: 3px 5px; box-sizing: border-box; display: flex; align-items: center; justify-content: center; flex: none; opacity: 0; color: inherit; border-radius: 4px; transition: opacity .1s ease, color .1s ease, background .1s ease; }
.vela-dtb-star svg { width: 16px; height: 16px; }
.vela-dtb-item:hover .vela-dtb-star, .vela-dtb-star:focus-visible { opacity: .55; }
.vela-dtb-star:hover { opacity: 1 !important; background: var(--dtb-hover-strong); }
.vela-dtb-star.vela-fav { opacity: .95; color: var(--dtb-gold); }


.vela-dmenu { position: absolute; z-index: 25; display: flex; flex-direction: column; gap: 2px; padding: 4px; border-radius: 8px; background: var(--dtb-bg); border: 1px solid var(--dtb-border); box-shadow: 0 6px 20px rgba(0,0,0,.35); color: var(--dtb-fg); pointer-events: auto; }

/* ── the settings popup: the quick bar and its menus ── */
.vela-dpop { position: absolute; z-index: 22; display: flex; flex-direction: column; background: var(--dlg-bg); color: var(--dlg-fg); border: 1px solid var(--dlg-border); border-radius: 8px; box-shadow: 0 6px 20px rgba(0,0,0,.35); font-size: 13px; pointer-events: auto; overflow: hidden; user-select: none; -webkit-user-select: none; cursor: default; box-sizing: border-box; }
.vela-dpop-row { display: flex; align-items: center; gap: 2px; padding: 4px; }
.vela-dpop button { font: inherit; }
.vela-dpop-btn { display: flex; align-items: center; justify-content: center; width: 30px; height: 30px; padding: 0; cursor: pointer; border: none; border-radius: 5px; background: transparent; color: var(--dlg-muted); transition: background .12s ease, color .12s ease; }
.vela-dpop-btn:hover { background: var(--dlg-hover-strong); color: var(--dlg-bright); }
.vela-dpop-btn[data-active='1'] { background: var(--dlg-active); color: var(--dlg-bright); }
.vela-dpop-btn[data-active='mixed'] { background: var(--dlg-hover-strong); color: var(--dlg-bright); outline: 1px dashed var(--dlg-muted); outline-offset: -2px; }
.vela-dpop-btn:focus-visible, .vela-dpop-grip:focus-visible, .vela-dpop-item:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: -2px; }
.vela-dpop-grip { display: flex; align-items: center; justify-content: center; width: 30px; height: 30px; padding: 0; background: transparent; border: none; cursor: grab; touch-action: none; color: color-mix(in srgb, var(--dlg-fg) 40%, transparent); }
@media (pointer: coarse) { .vela-dpop-grip { width: 44px; } }
.vela-dpop-g { display: flex; }
.vela-dpop-g--row { flex: none; width: 22px; justify-content: center; }
.vela-dpop-dash { min-width: 18px; text-align: center; opacity: .85; }
.vela-dpop-lab { font-size: 11px; opacity: .85; font-variant-numeric: tabular-nums; }
.vela-dpop-lab--row { flex: 1; font-size: inherit; opacity: 1; }
.vela-dpop-chev { display: flex; opacity: .55; }
.vela-dpop-under { display: block; height: 3px; border-radius: 2px; }
.vela-dpop-sep { width: 1px; height: 18px; margin: 0 3px; background: var(--dlg-border); }
.vela-dpop-tip { position: absolute; z-index: 27; pointer-events: none; white-space: nowrap; background: var(--dlg-fg); color: var(--dlg-bg); padding: 3px 7px; border-radius: 5px; font: 12px var(--vela-chrome-font-family, sans-serif); box-shadow: 0 6px 20px rgba(0,0,0,.35); }
.vela-dpop-float { position: absolute; display: flex; flex-direction: column; gap: 1px; box-sizing: border-box; background: var(--dlg-bg); color: var(--dlg-fg); border: 1px solid var(--dlg-border); border-radius: 8px; box-shadow: 0 6px 20px rgba(0,0,0,.35); font: 13px var(--vela-chrome-font-family, sans-serif); pointer-events: auto; }
.vela-dpop-item { display: flex; align-items: center; gap: 8px; padding: 5px 8px; border: none; border-radius: 5px; background: transparent; color: inherit; cursor: pointer; text-align: start; font: inherit; font-variant-numeric: tabular-nums; }
.vela-dpop-item:hover { background: var(--dlg-hover-strong); }
.vela-dpop-item[data-active='1'] { background: var(--dlg-active); }
.vela-dpop-item--action { min-width: 150px; gap: 9px; padding: 6px 9px; }
.vela-dpop-inuse { display: flex; align-items: center; gap: 6px; padding-bottom: 9px; border-bottom: 1px solid var(--dlg-border); }
.vela-dpop-inuse-l { margin-inline-end: 2px; font-size: 11px; opacity: .7; }
.vela-dpop-chip { width: 18px; height: 18px; padding: 0; border: 1px solid var(--dlg-strong); border-radius: 4px; cursor: pointer; }
.vela-dpop-text { display: flex; flex-direction: column; gap: 4px; padding: 6px; border-top: 1px solid var(--dlg-border); }
.vela-dpop-tools { display: flex; align-items: center; transform: scale(.8); transform-origin: left center; }
.vela-dpop-num .vela-ctl-num { width: 64px; height: 28px; }

/* ── the settings panel ── */
.vela-dsp-scope [hidden] { display: none !important; }
.vela-dsp-pos { position: absolute; inset: 0; z-index: 24; display: flex; align-items: center; box-sizing: border-box; pointer-events: none; }
.vela-dsp-pos .vela-dsp { pointer-events: auto; }
.vela-dlg-panel.vela-dsp { width: 360px; min-width: 0; max-width: calc(100% - 24px); max-height: calc(100% - 24px); font-size: 13px; }
.vela-dsp .vela-dlg-header { gap: 6px; padding: 8px 8px 8px 16px; }
.vela-dsp .vela-dlg-title { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 15px; line-height: 22px; }
.vela-dsp .vela-dlg-footer { justify-content: flex-end; padding: 10px 12px; border-top: 1px solid var(--dlg-border); }
.vela-dsp-scroll { flex: 1 1 auto; min-height: 0; overflow-x: hidden; overflow-y: auto; }
.vela-dsp-block { display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; }
.vela-dsp-block + .vela-dsp-block, .vela-dsp-fold { border-top: 1px solid var(--dlg-border); }
.vela-dsp-sub { display: flex; flex-direction: column; gap: 8px; }
.vela-dsp-h { font-size: 11px; font-weight: 600; letter-spacing: .05em; text-transform: uppercase; color: var(--dlg-muted); }
.vela-dsp-row { display: flex; align-items: center; gap: 8px; min-height: 30px; }
.vela-dsp-lab { flex: 0 0 96px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--dlg-muted); }
.vela-dsp-row > .vela-dsp-lab + * { min-width: 0; }
.vela-dsp-end { display: flex; flex: 1; justify-content: flex-end; }
.vela-dsp-fold-h { all: unset; box-sizing: border-box; display: flex; align-items: center; gap: 8px; width: 100%; padding: 11px 16px; cursor: pointer; }
.vela-dsp-fold-h:hover { background: var(--dlg-hover); }
.vela-dsp-fold-h:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: -2px; }
.vela-dsp-fold-t { font-weight: 600; color: var(--dlg-bright); white-space: nowrap; }
.vela-dsp-fold-s { flex: 1; min-width: 0; overflow: hidden; text-align: end; text-overflow: ellipsis; white-space: nowrap; color: var(--dlg-muted); font-variant-numeric: tabular-nums; }
.vela-dsp-chev { display: flex; color: var(--dlg-muted); transition: transform .12s ease; }
.vela-dsp-fold[data-open] .vela-dsp-chev { transform: rotate(90deg); }
.vela-dsp-fold-b { display: flex; flex-direction: column; gap: 8px; padding: 0 16px 12px; }
.vela-dsp-fold:not([data-open]) .vela-dsp-fold-b { display: none; }
.vela-dsp-seg { display: inline-flex; flex: none; overflow: hidden; border: 1px solid var(--dlg-strong); border-radius: var(--dlg-radius); }
.vela-dsp-seg[data-full] { display: flex; flex: 1; }
.vela-dsp-seg > button { all: unset; box-sizing: border-box; display: flex; align-items: center; justify-content: center; min-width: 32px; height: 28px; padding: 0 8px; cursor: pointer; white-space: nowrap; color: var(--dlg-muted); }
.vela-dsp-seg[data-full] > button { flex: 1 1 0; min-width: 0; }
.vela-dsp-seg > button + button { border-inline-start: 1px solid var(--dlg-border); }
.vela-dsp-seg > button:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-dsp-seg > button[aria-pressed='true'] { background: var(--dlg-active); color: var(--dlg-bright); }
.vela-dsp-seg > button:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: -2px; }
.vela-dsp-chips { display: flex; flex-wrap: wrap; gap: 6px; }
.vela-dsp-chip { all: unset; box-sizing: border-box; display: inline-flex; align-items: center; gap: 6px; height: 26px; padding: 0 10px; cursor: pointer; white-space: nowrap; color: var(--dlg-muted); border: 1px solid var(--dlg-strong); border-radius: 13px; }
.vela-dsp-chip:hover { color: var(--dlg-bright); border-color: var(--dlg-muted); }
.vela-dsp-chip[aria-pressed='true'] { background: var(--dlg-active); border-color: transparent; color: var(--dlg-bright); }
.vela-dsp-chip:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: 1px; }
.vela-dsp-note { font-size: 12px; line-height: 1.45; color: var(--dlg-muted); }
.vela-dsp-summary { padding-top: 6px; border-top: 1px solid var(--dlg-border); font-variant-numeric: tabular-nums; }
.vela-dsp-pt { display: grid; grid-template-columns: 34px minmax(0, 120px) minmax(0, 1fr); align-items: center; gap: 8px; min-height: 30px; }
.vela-dsp-pt-k { font-weight: 600; color: var(--dlg-muted); }
.vela-dsp-pt-t { overflow: hidden; text-align: end; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; color: var(--dlg-muted); font-variant-numeric: tabular-nums; }
.vela-dsp-pt .vela-ctl-num { width: 100%; }
.vela-dsp-tf { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 4px; }
.vela-dsp-tf > button { all: unset; box-sizing: border-box; position: relative; display: flex; align-items: center; justify-content: center; height: 28px; cursor: pointer; color: var(--dlg-muted); border: 1px solid var(--dlg-strong); border-radius: var(--dlg-radius); font-variant-numeric: tabular-nums; }
.vela-dsp-tf > button:hover { color: var(--dlg-bright); border-color: var(--dlg-muted); }
.vela-dsp-tf > button[aria-pressed='true'] { background: var(--dlg-active); border-color: transparent; color: var(--dlg-bright); }
.vela-dsp-tf > button[data-here]::after { content: ''; position: absolute; top: 3px; inset-inline-end: 3px; width: 5px; height: 5px; border-radius: 50%; background: var(--dlg-fg); }
.vela-dsp-tf > button:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: -2px; }
.vela-dsp-lv { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px 14px; }
.vela-dsp-lv-c { display: flex; align-items: center; gap: 6px; min-width: 0; }
.vela-dsp-lv-c .vela-ctl-num { width: 76px; height: 28px; }
.vela-dsp-lv-c[data-off] .vela-ctl-num { color: var(--dlg-muted); }
.vela-dsp-lvl { display: flex; align-items: center; gap: 8px; }
.vela-dsp-lvl-l { flex: 1; }
.vela-dsp-style { all: unset; box-sizing: border-box; display: inline-flex; flex: none; align-items: center; gap: 4px; max-width: 150px; height: 28px; padding: 0 6px 0 10px; cursor: pointer; color: var(--dlg-fg); border: 1px solid var(--dlg-strong); border-radius: var(--dlg-radius); font-size: 12px; }
.vela-dsp-style > span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-dsp-style:hover, .vela-dsp-style[aria-expanded='true'] { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-dsp-style:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: 1px; }
.vela-dsp-style-c { display: flex; }
.vela-dsp-menu { min-width: 220px; max-height: 320px; overflow-y: auto; padding: 4px; }
.vela-dsp-mh { padding: 6px 8px 4px; font-size: 11px; font-weight: 600; letter-spacing: .05em; text-transform: uppercase; color: var(--dlg-muted); }
.vela-dsp-mi { all: unset; box-sizing: border-box; display: flex; align-items: center; gap: 8px; height: 30px; padding: 0 4px 0 8px; cursor: pointer; color: inherit; border-radius: 5px; }
.vela-dsp-mi:hover, .vela-dsp-mi:focus-visible { background: var(--dlg-hover-strong); }
.vela-dsp-mi[data-current] { color: var(--dlg-bright); font-weight: 600; }
.vela-dsp-mi-t { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-dsp-mi-dot { flex: none; width: 14px; height: 14px; border: 1px solid var(--dlg-strong); border-radius: 3px; }
.vela-dsp-mi-x { all: unset; display: flex; flex: none; align-items: center; justify-content: center; width: 24px; height: 24px; cursor: pointer; color: var(--dlg-muted); border-radius: 3px; }
.vela-dsp-mi-x:hover { background: var(--dlg-hover); color: var(--dlg-danger); }
.vela-dsp-msep { height: 1px; margin: 4px 0; background: var(--dlg-border); }
.vela-dsp-mname { display: flex; gap: 6px; padding: 4px; }
.vela-dsp-mname > input { flex: 1; min-width: 0; box-sizing: border-box; height: 28px; padding: 0 8px; color: inherit; background: transparent; border: 1px solid var(--dlg-strong); border-radius: var(--dlg-radius); font: inherit; outline: none; }
.vela-dsp-mname > input:focus { border-color: var(--dlg-fg); }
[data-vela-layout='mobile'] .vela-dsp-pos { padding: 0 !important; }
[data-vela-layout='mobile'] .vela-dlg-panel.vela-dsp { position: relative !important; inset: auto !important; width: 100%; max-width: none; height: 100%; max-height: none; border: none; border-radius: 0; transform: none !important; }

}
"#;

/// Install the stylesheet once per document.
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

/// Force an inline SVG icon to fill its slot (the original's `sizedIcon`): the new
/// attributes come first, and HTML keeps the first of a duplicated one.
pub fn sized_icon(svg: &str) -> String {
    svg.replacen("<svg ", "<svg width=\"100%\" height=\"100%\" ", 1)
}

/// A glyph in a centered, fixed-size span.
fn icon_span(svg: &str, size: u32) -> String {
    if svg.is_empty() {
        return String::new();
    }
    format!(
        "<span style=\"width:{size}px;height:{size}px;display:flex;align-items:center;justify-content:center;\">{}</span>",
        sized_icon(svg)
    )
}

/// A button's inner content: the glyph inside the centered square that carries the
/// hover / active tint, so the tint hugs the icon instead of the whole row.
pub fn hit_html(svg: &str, icon_size: u32) -> String {
    format!(
        "<span class=\"vela-dtb-hit\">{}</span>",
        icon_span(svg, icon_size)
    )
}

/// The chevron's inner content: the same tint, in a narrow pill beside the icon.
pub fn arrow_hit_html(svg: &str) -> String {
    format!(
        "<span class=\"vela-dtb-hit vela-dtb-hit--arrow\">{}</span>",
        icon_span(svg, 11)
    )
}
