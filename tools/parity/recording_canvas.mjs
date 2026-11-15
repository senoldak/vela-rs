// A stand-in for `CanvasRenderingContext2D` that paints nothing and records
// every call as `[name, arg…]`, the way `vela_core::render::canvas::RecordingCanvas`
// does on the Rust side: property writes under the property's own name, the same
// fixed text metric, the same argument lists. The original painters run on it and
// the two call streams are compared.

const PROPERTIES = ['fillStyle', 'strokeStyle', 'globalAlpha', 'lineWidth', 'lineCap', 'lineJoin', 'font', 'textAlign', 'textBaseline', 'direction'];

const DEFAULTS = {
    fillStyle: '#000000',
    strokeStyle: '#000000',
    globalAlpha: 1,
    lineWidth: 1,
    lineCap: 'butt',
    lineJoin: 'miter',
    font: '10px sans-serif',
    textAlign: 'start',
    textBaseline: 'alphabetic',
    direction: 'inherit',
};

/** The font size in px of a CSS font shorthand; 10 when it names none. */
export function fontSizePx(font) {
    const match = /(\d[\d.]*|\.\d+)px/.exec(String(font));
    const size = match ? Number.parseFloat(match[1]) : Number.NaN;
    return Number.isFinite(size) ? size : 10;
}

/** Every UTF-16 unit is 0.6 em wide, ascent 0.8 em, descent 0.2 em. */
export function recordedTextMetrics(text, font) {
    const size = fontSizePx(font);
    return { width: String(text).length * size * 0.6, actualBoundingBoxAscent: size * 0.8, actualBoundingBoxDescent: size * 0.2 };
}

class RecordedGradient {
    constructor(x0, y0, x1, y1) {
        this.line = [x0, y0, x1, y1];
        this.stops = [];
    }
    addColorStop(offset, color) {
        this.stops.push([offset, color]);
    }
    // Serialized when the stream is written, so stops added after the assignment count.
    toJSON() {
        return { linearGradient: this.line, stops: this.stops };
    }
}

// What a real context does with a value it would not accept (the write is still recorded).
function accepts(name, value) {
    if (name === 'globalAlpha') return Number.isFinite(value) && value >= 0 && value <= 1;
    if (name === 'lineWidth') return Number.isFinite(value) && value > 0;
    return true;
}

export class RecordingContext {
    constructor(canvas = { width: 0, height: 0 }) {
        this.canvas = canvas;
        this.ops = [];
        this._state = { ...DEFAULTS };
        this._stack = [];
        for (const name of PROPERTIES) {
            Object.defineProperty(this, name, {
                enumerable: true,
                get: () => this._state[name],
                set: (value) => {
                    if (accepts(name, value)) this._state[name] = value;
                    this.ops.push([name, value]);
                },
            });
        }
    }

    save() {
        this._stack.push({ ...this._state });
        this.ops.push(['save']);
    }
    restore() {
        const saved = this._stack.pop();
        if (saved) this._state = saved;
        this.ops.push(['restore']);
    }

    translate(x, y) {
        this.ops.push(['translate', x, y]);
    }
    rotate(angle) {
        this.ops.push(['rotate', angle]);
    }
    scale(x, y) {
        this.ops.push(['scale', x, y]);
    }
    setTransform(a, b, c, d, e, f) {
        this.ops.push(['setTransform', a, b, c, d, e, f]);
    }

    setLineDash(segments) {
        this.ops.push(['setLineDash', [...segments]]);
    }
    createLinearGradient(x0, y0, x1, y1) {
        return new RecordedGradient(x0, y0, x1, y1);
    }

    beginPath() {
        this.ops.push(['beginPath']);
    }
    moveTo(x, y) {
        this.ops.push(['moveTo', x, y]);
    }
    lineTo(x, y) {
        this.ops.push(['lineTo', x, y]);
    }
    closePath() {
        this.ops.push(['closePath']);
    }
    rect(x, y, w, h) {
        this.ops.push(['rect', x, y, w, h]);
    }
    roundRect(x, y, w, h, radii = 0) {
        // The port's surface takes one radius for every corner.
        const radius = Array.isArray(radii) && radii.length === 1 ? radii[0] : radii;
        this.ops.push(['roundRect', x, y, w, h, radius]);
    }
    arc(x, y, radius, start, end, counterclockwise = false) {
        this.ops.push(['arc', x, y, radius, start, end, Boolean(counterclockwise)]);
    }
    arcTo(x1, y1, x2, y2, radius) {
        this.ops.push(['arcTo', x1, y1, x2, y2, radius]);
    }
    ellipse(x, y, rx, ry, rotation, start, end, counterclockwise = false) {
        this.ops.push(['ellipse', x, y, rx, ry, rotation, start, end, Boolean(counterclockwise)]);
    }
    quadraticCurveTo(cpx, cpy, x, y) {
        this.ops.push(['quadraticCurveTo', cpx, cpy, x, y]);
    }
    bezierCurveTo(c1x, c1y, c2x, c2y, x, y) {
        this.ops.push(['bezierCurveTo', c1x, c1y, c2x, c2y, x, y]);
    }

    fill() {
        this.ops.push(['fill']);
    }
    stroke() {
        this.ops.push(['stroke']);
    }
    clip() {
        this.ops.push(['clip']);
    }
    fillRect(x, y, w, h) {
        this.ops.push(['fillRect', x, y, w, h]);
    }
    strokeRect(x, y, w, h) {
        this.ops.push(['strokeRect', x, y, w, h]);
    }
    clearRect(x, y, w, h) {
        this.ops.push(['clearRect', x, y, w, h]);
    }

    fillText(text, x, y) {
        this.ops.push(['fillText', String(text), x, y]);
    }
    strokeText(text, x, y) {
        this.ops.push(['strokeText', String(text), x, y]);
    }
    measureText(text) {
        return recordedTextMetrics(text, this._state.font);
    }

    drawImage(_image, dx, dy, dw, dh) {
        this.ops.push(['drawImage', dx, dy, dw, dh]);
    }

    /** The recorded stream as plain JSON values (non-finite numbers become null, as in Rust's writer). */
    toJSON() {
        return JSON.parse(JSON.stringify(this.ops));
    }
}
