//! Best-effort rasterization of the DOM overlays into a screenshot: the original's
//! `dom-raster.ts` (`rasterizeOverlay`) and the overlay passes of `screenshotCanvas`.
//!
//! What a chart overlay carries is text on (rounded) color chips, and that subset rasterizes
//! with plain canvas calls from the live layout: element backgrounds and borders (a `50%`
//! radius reads as a circle), text nodes in their computed font and color at their measured
//! rects, already-loaded images (only when drawing them leaves the canvas untainted), and
//! inline SVG icons through path / circle / ellipse / rect / line / polyline / polygon. An
//! element carrying a `transform` is skipped: honest absence beats a wrongly placed glyph.
//! Hidden elements (`display: none`, `visibility: hidden`, zero-size boxes) are skipped and
//! opacity multiplies down. One deliberate difference: an explicit `opacity: 0` stays 0 (the
//! original's `parseFloat(..) || 1` read it as 1 and painted the element).
//!
//! Which elements join: every `[data-vela-pane]` (the indicator legends) and every element
//! under the chart's container marked `data-vela-screenshot` (the status line, the
//! attribution mark = `1`; the symbol watermark = `under`, drawn beneath the canvases).
//! Only the crosshair canvas is excluded from a screenshot.
//!
//! The port is a script rather than web-sys calls: every property it reads (computed style,
//! ranges, `viewBox`) is a stringly-typed DOM read.

use vela_core::workspace::screenshot::OverlayPhase;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, Element};

#[wasm_bindgen(inline_js = r#"
function hasInk(color) {
    if (!color || color === 'transparent') return false;
    const m = /rgba?\(\s*[\d.]+\s*,\s*[\d.]+\s*,\s*[\d.]+\s*,\s*([\d.]+)\s*\)/.exec(color);
    return m ? parseFloat(m[1]) > 0.001 : true;
}

// An opacity: a missing or unreadable value is 1, an explicit 0 stays 0 (the original's
// `|| 1` turned 0 into 1, which painted fully transparent elements).
function num01(v) {
    const n = parseFloat(v);
    return Number.isNaN(n) ? 1 : n;
}

function cornerRadius(cs, w, h) {
    const raw = cs.borderTopLeftRadius || '';
    const v = parseFloat(raw) || 0;
    const px = raw.endsWith('%') ? (v / 100) * Math.min(w, h) : v;
    return Math.max(0, Math.min(px, Math.min(w, h) / 2));
}

function tracePath(ctx, x, y, w, h, r) {
    ctx.beginPath();
    if (r > 0 && typeof ctx.roundRect === 'function') ctx.roundRect(x, y, w, h, r);
    else ctx.rect(x, y, w, h);
}

// A cross-origin image without CORS taints whatever it lands on; test it in isolation.
function canDrawUntainted(img) {
    try {
        const probe = img.ownerDocument.createElement('canvas');
        probe.width = 1;
        probe.height = 1;
        const pctx = probe.getContext('2d');
        if (!pctx) return false;
        pctx.drawImage(img, 0, 0, 1, 1);
        probe.toDataURL();
        return true;
    } catch (_) {
        return false;
    }
}

// The overlay elements of `container` for one phase, in drawing order. `under`: the
// elements marked 'under'. `over`: every legend column, then the other marked elements.
export function vela_raster_collect(container, over) {
    const out = [];
    try {
        const marked = Array.from(container.querySelectorAll('[data-vela-screenshot]'));
        if (!over) {
            for (const el of marked) if (el.getAttribute('data-vela-screenshot') === 'under') out.push(el);
            return out;
        }
        for (const el of container.querySelectorAll('[data-vela-pane]')) out.push(el);
        for (const el of marked) if (el.getAttribute('data-vela-screenshot') !== 'under') out.push(el);
    } catch (_) {
        // best effort
    }
    return out;
}

// The plot's viewport box as [left, top, width, height].
export function vela_raster_rect(el) {
    const r = el.getBoundingClientRect();
    return [r.left, r.top, r.width, r.height];
}

// Draw `root`'s subtree onto `ctx`: `left`/`top` is the viewport point the canvas origin
// stands for, `dpr` the canvas pixels per CSS pixel. Never throws: a failing element is skipped.
export function vela_raster_overlay(ctx, root, left, top, dpr) {
    const win = root.ownerDocument && root.ownerDocument.defaultView;
    if (!win) return;
    ctx.save();
    try {
        ctx.scale(dpr, dpr);

        const drawText = (node, cs, alpha) => {
            const text = (node.textContent || '').replace(/\s+/g, ' ').trim();
            if (!text) return;
            const range = node.ownerDocument.createRange();
            range.selectNodeContents(node);
            const r = range.getBoundingClientRect();
            if (r.width <= 0 || r.height <= 0) return;
            ctx.globalAlpha = alpha;
            ctx.fillStyle = cs.color;
            ctx.font = cs.fontStyle + ' ' + cs.fontWeight + ' ' + cs.fontSize + ' ' + cs.fontFamily;
            ctx.textBaseline = 'middle';
            ctx.textAlign = 'left';
            ctx.fillText(text, r.left - left, r.top - top + r.height / 2);
        };

        const drawImg = (img, cs, alpha) => {
            if (!img.complete || img.naturalWidth <= 0 || !canDrawUntainted(img)) return;
            const r = img.getBoundingClientRect();
            if (r.width <= 0 || r.height <= 0) return;
            const x = r.left - left;
            const y = r.top - top;
            ctx.save();
            ctx.globalAlpha = alpha;
            tracePath(ctx, x, y, r.width, r.height, cornerRadius(cs, r.width, r.height));
            ctx.clip();
            ctx.drawImage(img, x, y, r.width, r.height);
            ctx.restore();
        };

        const drawSvgShapes = (parent, alpha) => {
            for (const el of parent.children) {
                if (el.hasAttribute('transform')) continue;
                const tag = el.tagName.toLowerCase();
                if (tag === 'g') {
                    drawSvgShapes(el, alpha);
                    continue;
                }
                const num = (name) => parseFloat(el.getAttribute(name) || '0') || 0;
                let path = null;
                if (tag === 'path') {
                    const d = el.getAttribute('d');
                    if (d) path = new Path2D(d);
                } else if (tag === 'circle') {
                    path = new Path2D();
                    path.arc(num('cx'), num('cy'), num('r'), 0, Math.PI * 2);
                } else if (tag === 'ellipse') {
                    path = new Path2D();
                    path.ellipse(num('cx'), num('cy'), num('rx'), num('ry'), 0, 0, Math.PI * 2);
                } else if (tag === 'rect') {
                    path = new Path2D();
                    path.rect(num('x'), num('y'), num('width'), num('height'));
                } else if (tag === 'line') {
                    path = new Path2D();
                    path.moveTo(num('x1'), num('y1'));
                    path.lineTo(num('x2'), num('y2'));
                } else if (tag === 'polyline' || tag === 'polygon') {
                    const pts = (el.getAttribute('points') || '').trim().split(/[\s,]+/).map(Number);
                    if (pts.length < 4 || pts.some(Number.isNaN)) continue;
                    path = new Path2D();
                    path.moveTo(pts[0], pts[1]);
                    for (let i = 2; i + 1 < pts.length; i += 2) path.lineTo(pts[i], pts[i + 1]);
                    if (tag === 'polygon') path.closePath();
                }
                if (!path) continue;
                const cs = win.getComputedStyle(el);
                if (cs.fill && cs.fill !== 'none' && hasInk(cs.fill)) {
                    ctx.globalAlpha = alpha * (num01(cs.fillOpacity));
                    ctx.fillStyle = cs.fill;
                    ctx.fill(path);
                }
                const sw = parseFloat(cs.strokeWidth) || 0;
                if (cs.stroke && cs.stroke !== 'none' && hasInk(cs.stroke) && sw > 0) {
                    ctx.globalAlpha = alpha * (num01(cs.strokeOpacity));
                    ctx.strokeStyle = cs.stroke;
                    ctx.lineWidth = sw;
                    if (cs.strokeLinecap === 'round' || cs.strokeLinecap === 'square' || cs.strokeLinecap === 'butt') ctx.lineCap = cs.strokeLinecap;
                    if (cs.strokeLinejoin === 'round' || cs.strokeLinejoin === 'bevel' || cs.strokeLinejoin === 'miter') ctx.lineJoin = cs.strokeLinejoin;
                    ctx.stroke(path);
                }
            }
        };

        const drawSvg = (svg, parentAlpha) => {
            const r = svg.getBoundingClientRect();
            if (r.width <= 0 || r.height <= 0) return;
            const cs = win.getComputedStyle(svg);
            if (cs.display === 'none' || cs.visibility === 'hidden') return;
            const alpha = parentAlpha * (num01(cs.opacity));
            if (alpha <= 0.001) return;
            const vb = svg.viewBox && svg.viewBox.baseVal;
            const vw = vb && vb.width > 0 ? vb.width : r.width;
            const vh = vb && vb.height > 0 ? vb.height : r.height;
            ctx.save();
            ctx.translate(r.left - left, r.top - top);
            ctx.scale(r.width / vw, r.height / vh);
            if (vb) ctx.translate(-vb.x, -vb.y);
            drawSvgShapes(svg, alpha);
            ctx.restore();
        };

        const drawElement = (el, parentAlpha) => {
            const cs = win.getComputedStyle(el);
            if (cs.display === 'none' || cs.visibility === 'hidden') return;
            const alpha = parentAlpha * (num01(cs.opacity));
            if (alpha <= 0.001) return;
            const r = el.getBoundingClientRect();
            if (r.width > 0 && r.height > 0) {
                const x = r.left - left;
                const y = r.top - top;
                const radius = cornerRadius(cs, r.width, r.height);
                if (hasInk(cs.backgroundColor)) {
                    ctx.globalAlpha = alpha;
                    ctx.fillStyle = cs.backgroundColor;
                    tracePath(ctx, x, y, r.width, r.height, radius);
                    ctx.fill();
                }
                const bw = parseFloat(cs.borderTopWidth) || 0;
                if (bw > 0 && cs.borderTopStyle !== 'none' && hasInk(cs.borderTopColor)) {
                    ctx.globalAlpha = alpha;
                    ctx.strokeStyle = cs.borderTopColor;
                    ctx.lineWidth = bw;
                    tracePath(ctx, x + bw / 2, y + bw / 2, r.width - bw, r.height - bw, Math.max(0, radius - bw / 2));
                    ctx.stroke();
                }
            }
            for (const child of el.childNodes) {
                if (child.nodeType === 3) drawText(child, cs, alpha);
                else if (child instanceof win.HTMLImageElement) drawImg(child, win.getComputedStyle(child), alpha);
                else if (child instanceof win.SVGSVGElement) drawSvg(child, alpha);
                else if (child instanceof win.HTMLElement) drawElement(child, alpha);
            }
        };

        if (root instanceof win.HTMLElement) drawElement(root, 1);
    } catch (_) {
        // a failing overlay must not lose the screenshot
    } finally {
        ctx.restore();
    }
}
"#)]
extern "C" {
    fn vela_raster_collect(container: &Element, over: bool) -> js_sys::Array;
    fn vela_raster_rect(el: &Element) -> js_sys::Array;
    fn vela_raster_overlay(
        ctx: &CanvasRenderingContext2d,
        root: &Element,
        left: f64,
        top: f64,
        dpr: f64,
    );
}

/// Rasterize the overlays of `phase` that live under `container` into the tile whose
/// top-left is `(dx, dy)` and width `dw` (canvas pixels) on `ctx`. `plot` is the element the
/// tile's canvases cover; its on-screen box maps CSS coordinates onto the tile (the export
/// canvas of the original is `plot` at `out.width / plot.width` device pixels per CSS pixel).
pub fn paint_overlays(
    ctx: &CanvasRenderingContext2d,
    container: &Element,
    plot: &Element,
    phase: OverlayPhase,
    (dx, dy, dw): (f64, f64, f64),
) {
    let rect = vela_raster_rect(plot);
    let num = |i: u32| rect.get(i).as_f64().unwrap_or(0.0);
    let (left, top, width) = (num(0), num(1), num(2));
    if width <= 0.0 || dw <= 0.0 {
        return;
    }
    let dpr = dw / width;
    let elements = vela_raster_collect(container, phase == OverlayPhase::Over);
    for item in elements.iter() {
        let Ok(el) = item.dyn_into::<Element>() else {
            continue;
        };
        ctx.save();
        let _ = ctx.translate(dx, dy);
        vela_raster_overlay(ctx, &el, left, top, dpr);
        ctx.restore();
    }
}
