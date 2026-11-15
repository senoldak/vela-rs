//! A growable triangle batch: interleaved `[x, y, r, g, b, a, edgeDist, edgeHalf]`
//! vertices in CSS-pixel space, drawn by one per-vertex-color shader. Every
//! primitive the geometry layer needs decomposes into colored triangles here, and a
//! vertical gradient is just a quad whose top/bottom vertices carry different colors
//! (the GPU interpolates it), so no gradient shader is required.
//!
//! `edgeDist` / `edgeHalf` drive analytic line anti-aliasing: for a line segment
//! they carry the signed perpendicular distance from the centerline and the line's
//! half-width, so the fragment shader can feather the edge over about one device
//! pixel (crisp, independent of the driver's MSAA). All other primitives push
//! `edgeHalf < 0`, the sentinel for a solid fill with no feathering.
//!
//! The vertex data is `f32`, as the original's `Float32Array`: every coordinate and
//! color is narrowed (round to nearest even) when it is pushed. The arithmetic
//! before that is `f64`, as JavaScript's.

use super::color::Rgba;

/// Floats per vertex: x, y, r, g, b, a, edgeDist, edgeHalf.
pub const FLOATS_PER_VERT: usize = 8;
/// Perpendicular padding added to each side of a line quad so the ~1px edge feather
/// has room.
const AA_PAD: f64 = 1.0;

/// `Math.hypot(a, b)` the way V8 computes it (scaled Kahan sum), which can differ
/// from a libm `hypot` in the last bit.
pub fn js_hypot(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return if a.is_infinite() || b.is_infinite() {
            f64::INFINITY
        } else {
            f64::NAN
        };
    }
    let (a, b) = (a.abs(), b.abs());
    let max = if a > b { a } else { b };
    if max == f64::INFINITY {
        return f64::INFINITY;
    }
    if max == 0.0 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut compensation = 0.0;
    for v in [a, b] {
        let n = v / max;
        let summand = n * n - compensation;
        let preliminary = sum + summand;
        compensation = (preliminary - sum) - summand;
        sum = preliminary;
    }
    sum.sqrt() * max
}

/// The fdlibm `cos`/`sin` (what V8's `Math.cos` / `Math.sin` are), for the arguments the
/// circle fans use: `0 <= x <= 7`. The hardware libm of a native build differs from it in
/// the last bits (the MSVC runtime is off by 5e-5 relative around cos(pi/2)), while
/// the wasm build's libm is fdlibm too; computing here makes the vertex data the original's
/// on every target. Anything outside that range falls back to the platform's function.
mod fdlibm {
    // The constants are fdlibm's own digits.
    #![allow(clippy::excessive_precision, clippy::approx_constant)]

    const INVPIO2: f64 = 6.366_197_723_675_813_824_33e-1;
    const PIO2_1: f64 = 1.570_796_326_734_125_614_17;
    const PIO2_1T: f64 = 6.077_100_506_506_192_249_32e-11;
    const PIO2_2: f64 = 6.077_100_506_303_965_976_60e-11;
    const PIO2_2T: f64 = 2.022_266_248_795_950_631_54e-21;
    const PIO2_3: f64 = 2.022_266_248_711_166_455_80e-21;
    const PIO2_3T: f64 = 8.478_427_660_368_899_569_97e-32;
    /// High words of `n * pi / 2` for `n = 1..=4` (fdlibm's `npio2_hw`).
    const NPIO2_HW: [u32; 4] = [0x3FF9_21FB, 0x4009_21FB, 0x4012_D97C, 0x4019_21FB];

    fn high(x: f64) -> u32 {
        (x.to_bits() >> 32) as u32
    }

    fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
        const S1: f64 = -1.666_666_666_666_663_243_48e-1;
        const S2: f64 = 8.333_333_333_322_489_461_24e-3;
        const S3: f64 = -1.984_126_982_985_794_931_34e-4;
        const S4: f64 = 2.755_731_370_707_006_767_89e-6;
        const S5: f64 = -2.505_076_025_340_686_341_95e-8;
        const S6: f64 = 1.589_690_995_211_550_102_21e-10;
        let ix = high(x) & 0x7fff_ffff;
        if ix < 0x3e40_0000 && x as i32 == 0 {
            return x;
        }
        let z = x * x;
        let v = z * x;
        let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
        if iy == 0 {
            x + v * (S1 + z * r)
        } else {
            x - ((z * (0.5 * y - v * r) - y) - v * S1)
        }
    }

    fn kernel_cos(x: f64, y: f64) -> f64 {
        const C1: f64 = 4.166_666_666_666_660_190_37e-2;
        const C2: f64 = -1.388_888_888_887_410_957_49e-3;
        const C3: f64 = 2.480_158_728_947_672_941_78e-5;
        const C4: f64 = -2.755_731_435_139_066_330_35e-7;
        const C5: f64 = 2.087_572_321_298_174_827_90e-9;
        const C6: f64 = -1.135_964_755_778_819_482_65e-11;
        let ix = high(x) & 0x7fff_ffff;
        if ix < 0x3e40_0000 && x as i32 == 0 {
            return 1.0;
        }
        let z = x * x;
        let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
        if ix < 0x3FD3_3333 {
            1.0 - (0.5 * z - (z * r - x * y))
        } else {
            let qx = if ix > 0x3fe9_0000 {
                0.28125
            } else {
                f64::from_bits(u64::from(ix - 0x0020_0000) << 32)
            };
            let hz = 0.5 * z - qx;
            let a = 1.0 - qx;
            a - (hz - (z * r - x * y))
        }
    }

    /// `__ieee754_rem_pio2` for `0 < x <= 7`: the quadrant `n` and `x - n * pi / 2` as
    /// a head and a tail.
    fn rem_pio2(x: f64) -> (i32, f64, f64) {
        let ix = high(x) & 0x7fff_ffff;
        if ix < 0x4002_d97c {
            // |x| < 3pi/4: special case n = 1
            let mut z = x - PIO2_1;
            if ix != 0x3ff9_21fb {
                let y0 = z - PIO2_1T;
                return (1, y0, (z - y0) - PIO2_1T);
            }
            z -= PIO2_2;
            let y0 = z - PIO2_2T;
            return (1, y0, (z - y0) - PIO2_2T);
        }
        // medium size
        let t = x;
        let n = (t * INVPIO2 + 0.5) as i32;
        let fnn = f64::from(n);
        let mut r = t - fnn * PIO2_1;
        let mut w = fnn * PIO2_1T; // 1st round good to 85 bit
        let mut y0;
        if n < 32 && ix != NPIO2_HW[(n - 1) as usize] {
            y0 = r - w; // quick check no cancellation
        } else {
            let j = (ix >> 20) as i32;
            y0 = r - w;
            let mut i = j - ((high(y0) >> 20) & 0x7ff) as i32;
            if i > 16 {
                // 2nd iteration needed, good to 118
                let t2 = r;
                w = fnn * PIO2_2;
                r = t2 - w;
                w = fnn * PIO2_2T - ((t2 - r) - w);
                y0 = r - w;
                i = j - ((high(y0) >> 20) & 0x7ff) as i32;
                if i > 49 {
                    // 3rd iteration need, 151 bits acc
                    let t3 = r;
                    w = fnn * PIO2_3;
                    r = t3 - w;
                    w = fnn * PIO2_3T - ((t3 - r) - w);
                    y0 = r - w;
                }
            }
        }
        (n, y0, (r - y0) - w)
    }

    fn in_domain(x: f64) -> bool {
        x > 0.0 && x <= 7.0
    }

    pub fn cos(x: f64) -> f64 {
        if !in_domain(x) {
            return x.cos();
        }
        if high(x) & 0x7fff_ffff <= 0x3fe9_21fb {
            return kernel_cos(x, 0.0);
        }
        let (n, y0, y1) = rem_pio2(x);
        match n & 3 {
            0 => kernel_cos(y0, y1),
            1 => -kernel_sin(y0, y1, 1),
            2 => -kernel_cos(y0, y1),
            _ => kernel_sin(y0, y1, 1),
        }
    }

    pub fn sin(x: f64) -> f64 {
        if !in_domain(x) {
            return x.sin();
        }
        if high(x) & 0x7fff_ffff <= 0x3fe9_21fb {
            return kernel_sin(x, 0.0, 0);
        }
        let (n, y0, y1) = rem_pio2(x);
        match n & 3 {
            0 => kernel_sin(y0, y1, 1),
            1 => kernel_cos(y0, y1),
            2 => -kernel_sin(y0, y1, 1),
            _ => -kernel_cos(y0, y1),
        }
    }
}

pub struct Batch {
    buf: Vec<f32>,
    /// Alpha multiplier applied to every pushed vertex (1 = opaque): lets a backend
    /// fade a group of primitives, e.g. indicator models during the intro reveal.
    pub alpha: f64,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    pub fn new() -> Self {
        Self::with_capacity(4096)
    }

    /// A batch with room for `capacity_verts` vertices before it first grows.
    pub fn with_capacity(capacity_verts: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity_verts * FLOATS_PER_VERT),
            alpha: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.buf.clear();
    }

    pub fn vertex_count(&self) -> usize {
        self.buf.len() / FLOATS_PER_VERT
    }

    /// The used part of the buffer.
    pub fn view(&self) -> &[f32] {
        &self.buf
    }

    /// One vertex. `dist` / `half` default to the solid-fill sentinel (`half < 0`: no
    /// edge feathering); only the line builders pass real values.
    fn v(&mut self, x: f64, y: f64, c: Rgba, dist: f64, half: f64) {
        self.buf.extend_from_slice(&[
            x as f32,
            y as f32,
            c[0] as f32,
            c[1] as f32,
            c[2] as f32,
            (c[3] * self.alpha) as f32,
            dist as f32,
            half as f32,
        ]);
    }

    fn solid(&mut self, x: f64, y: f64, c: Rgba) {
        self.v(x, y, c, 0.0, -1.0);
    }

    /// Triangle with per-vertex color.
    #[allow(clippy::too_many_arguments)]
    pub fn tri(
        &mut self,
        x0: f64,
        y0: f64,
        c0: Rgba,
        x1: f64,
        y1: f64,
        c1: Rgba,
        x2: f64,
        y2: f64,
        c2: Rgba,
    ) {
        self.solid(x0, y0, c0);
        self.solid(x1, y1, c1);
        self.solid(x2, y2, c2);
    }

    /// Quad from 4 corners (any winding) with per-corner color: 2 triangles.
    #[allow(clippy::too_many_arguments)]
    pub fn quad4(
        &mut self,
        x0: f64,
        y0: f64,
        c0: Rgba,
        x1: f64,
        y1: f64,
        c1: Rgba,
        x2: f64,
        y2: f64,
        c2: Rgba,
        x3: f64,
        y3: f64,
        c3: Rgba,
    ) {
        self.solid(x0, y0, c0);
        self.solid(x1, y1, c1);
        self.solid(x2, y2, c2);
        self.solid(x0, y0, c0);
        self.solid(x2, y2, c2);
        self.solid(x3, y3, c3);
    }

    /// Single-color quad from 4 corners.
    #[allow(clippy::too_many_arguments)]
    pub fn quad(
        &mut self,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        x3: f64,
        y3: f64,
        c: Rgba,
    ) {
        self.quad4(x0, y0, c, x1, y1, c, x2, y2, c, x3, y3, c);
    }

    /// Axis-aligned rectangle.
    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: Rgba) {
        self.quad(x, y, x + w, y, x + w, y + h, x, y + h, c);
    }

    /// A line segment as a width-`w` quad (perpendicular expansion; no joins). The quad
    /// is padded by `AA_PAD` on each side and every vertex carries its signed
    /// perpendicular distance from the centerline plus the half-width, so the fragment
    /// shader feathers the edge analytically (crisp, MSAA-independent). The along-length
    /// butt caps are left hard (hidden at a polyline's joints).
    pub fn seg(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, w: f64, c: Rgba) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = js_hypot(dx, dy);
        if len < 1e-6 {
            return;
        }
        let hw = w / 2.0;
        let ext = hw + AA_PAD; // outer edge of the padded quad
        let ex = (-dy / len) * ext;
        let ey = (dx / len) * ext;
        // + side (dist = +ext): corners at (x0,y0)+e and (x1,y1)+e;  - side (dist = -ext): -e
        let (ax, ay) = (x0 + ex, y0 + ey);
        let (bx, by) = (x1 + ex, y1 + ey);
        let (cx, cy) = (x1 - ex, y1 - ey);
        let (dxc, dyc) = (x0 - ex, y0 - ey);
        self.v(ax, ay, c, ext, hw);
        self.v(bx, by, c, ext, hw);
        self.v(cx, cy, c, -ext, hw);
        self.v(ax, ay, c, ext, hw);
        self.v(cx, cy, c, -ext, hw);
        self.v(dxc, dyc, c, -ext, hw);
    }

    #[allow(clippy::too_many_arguments)]
    /// A dashed/dotted/solid segment. `pattern` = `[on_px, off_px]` or `None` for solid.
    pub fn dashed_seg(
        &mut self,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        w: f64,
        c: Rgba,
        pattern: Option<[f64; 2]>,
    ) {
        let Some(pattern) = pattern else {
            self.seg(x0, y0, x1, y1, w, c);
            return;
        };
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = js_hypot(dx, dy);
        if len < 1e-6 {
            return;
        }
        let ux = dx / len;
        let uy = dy / len;
        let period = pattern[0] + pattern[1];
        if period <= 0.0 {
            // Guard against a zero/negative pattern (no infinite loop).
            self.seg(x0, y0, x1, y1, w, c);
            return;
        }
        let mut d = 0.0;
        while d < len {
            let on = pattern[0].min(len - d);
            let a = d;
            let b = d + on;
            self.seg(x0 + ux * a, y0 + uy * a, x0 + ux * b, y0 + uy * b, w, c);
            d += period;
        }
    }

    /// The outline of an axis-aligned rectangle, stroked with width `line_width` (4
    /// edge segments).
    pub fn rect_stroke(&mut self, x: f64, y: f64, w: f64, h: f64, line_width: f64, c: Rgba) {
        self.seg(x, y, x + w, y, line_width, c); // top
        self.seg(x + w, y, x + w, y + h, line_width, c); // right
        self.seg(x + w, y + h, x, y + h, line_width, c); // bottom
        self.seg(x, y + h, x, y, line_width, c); // left
    }

    /// Filled circle (triangle fan), feathered like a line: the rim is padded by
    /// `AA_PAD` and edgeDist runs from 0 at the center to the padded radius at the rim,
    /// with edgeHalf = `r`. The distance grows linearly along each spoke, so the
    /// shader's line feather gives a round soft edge.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64, c: Rgba, segments: usize) {
        let ext = r + AA_PAD;
        let mut px = cx + ext;
        let mut py = cy;
        for i in 1..=segments {
            let a = (i as f64 / segments as f64) * std::f64::consts::PI * 2.0;
            let nx = cx + fdlibm::cos(a) * ext;
            let ny = cy + fdlibm::sin(a) * ext;
            self.v(cx, cy, c, 0.0, r);
            self.v(px, py, c, ext, r);
            self.v(nx, ny, c, ext, r);
            px = nx;
            py = ny;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = [1.0, 0.0, 0.0, 1.0];
    const BLUE: Rgba = [0.0, 0.0, 1.0, 1.0];
    const STRIDE: usize = FLOATS_PER_VERT;

    #[test]
    fn emits_6_vertices_per_rect_with_the_right_corners_and_color() {
        let mut b = Batch::with_capacity(2); // tiny initial capacity: exercises growth
        b.rect(2.0, 3.0, 10.0, 4.0, RED);
        assert_eq!(b.vertex_count(), 6);
        let v = b.view();
        assert_eq!(v.len(), 6 * STRIDE);
        // first vertex = top-left corner (2,3) with RED
        assert_eq!([v[0], v[1]], [2.0, 3.0]);
        assert_eq!([v[2], v[3], v[4], v[5]], [1.0, 0.0, 0.0, 1.0]);
        assert!(v[7] < 0.0); // edgeHalf < 0: solid fill, no edge feathering
        // the rect spans x in [2,12], y in [3,7]; every vertex within bounds
        for i in 0..b.vertex_count() {
            let (x, y) = (v[i * STRIDE], v[i * STRIDE + 1]);
            assert!((2.0..=12.0).contains(&x));
            assert!((3.0..=7.0).contains(&y));
        }
    }

    #[test]
    fn rect_vertex_order_is_two_triangles_from_the_top_left() {
        let mut b = Batch::new();
        b.rect(0.0, 0.0, 4.0, 2.0, RED);
        let xy: Vec<(f32, f32)> = b.view().chunks(STRIDE).map(|v| (v[0], v[1])).collect();
        assert_eq!(
            xy,
            vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 2.0),
                (0.0, 0.0),
                (4.0, 2.0),
                (0.0, 2.0)
            ]
        );
    }

    #[test]
    fn reset_clears_the_vertex_count_but_reuses_the_buffer() {
        let mut b = Batch::new();
        b.rect(0.0, 0.0, 1.0, 1.0, RED);
        b.reset();
        assert_eq!(b.vertex_count(), 0);
        b.tri(0.0, 0.0, RED, 1.0, 0.0, RED, 0.0, 1.0, RED);
        assert_eq!(b.vertex_count(), 3);
    }

    #[test]
    fn seg_feathers_a_line_into_a_padded_quad_and_skips_zero_length() {
        let mut b = Batch::new();
        b.seg(0.0, 0.0, 10.0, 0.0, 2.0, RED); // horizontal, width 2 (half 1), padded by 1: y in [-2, 2]
        assert_eq!(b.vertex_count(), 6);
        let v = b.view();
        for i in 0..6 {
            assert!((f64::from(v[i * STRIDE + 1]).abs() - 2.0).abs() < 1e-9); // padded outer edge
            assert!((f64::from(v[i * STRIDE + 6]).abs() - 2.0).abs() < 1e-9); // edgeDist = +-(half + pad)
            assert!((f64::from(v[i * STRIDE + 7]) - 1.0).abs() < 1e-9); // edgeHalf = the true half-width
        }
        b.reset();
        b.seg(5.0, 5.0, 5.0, 5.0, 2.0, RED); // zero length: nothing
        assert_eq!(b.vertex_count(), 0);
    }

    #[test]
    fn seg_vertex_layout() {
        let mut b = Batch::new();
        b.seg(0.0, 0.0, 10.0, 0.0, 2.0, RED);
        let rows: Vec<[f32; 4]> = b
            .view()
            .chunks(STRIDE)
            .map(|v| [v[0], v[1], v[6], v[7]])
            .collect();
        // normal = (-dy, dx)/len * ext = (0, 2)
        assert_eq!(
            rows,
            vec![
                [0.0, 2.0, 2.0, 1.0],
                [10.0, 2.0, 2.0, 1.0],
                [10.0, -2.0, -2.0, 1.0],
                [0.0, 2.0, 2.0, 1.0],
                [10.0, -2.0, -2.0, 1.0],
                [0.0, -2.0, -2.0, 1.0],
            ]
        );
    }

    #[test]
    fn quad4_carries_per_corner_colors() {
        let mut b = Batch::new();
        b.quad4(0.0, 0.0, RED, 1.0, 0.0, RED, 1.0, 1.0, BLUE, 0.0, 1.0, BLUE);
        assert_eq!(b.vertex_count(), 6);
        let colors: Vec<[f32; 4]> = b
            .view()
            .chunks(STRIDE)
            .map(|v| [v[2], v[3], v[4], v[5]])
            .collect();
        assert_eq!(colors[0], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(colors[2], [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(colors[5], [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn alpha_multiplies_every_pushed_vertex() {
        let mut b = Batch::new();
        b.alpha = 0.5;
        b.rect(0.0, 0.0, 1.0, 1.0, [1.0, 1.0, 1.0, 0.5]);
        assert!(b.view().chunks(STRIDE).all(|v| v[5] == 0.25));
    }

    #[test]
    fn dashed_seg_with_a_pattern_emits_several_dashes_and_solid_one() {
        let mut solid = Batch::new();
        solid.dashed_seg(0.0, 0.0, 100.0, 0.0, 1.0, RED, None);
        assert_eq!(solid.vertex_count(), 6); // one quad

        let mut dashed = Batch::new();
        dashed.dashed_seg(0.0, 0.0, 100.0, 0.0, 1.0, RED, Some([6.0, 4.0])); // 10px period
        assert_eq!(dashed.vertex_count(), 6 * 10);

        // the last dash is cut to the remaining length
        let mut short = Batch::new();
        short.dashed_seg(0.0, 0.0, 8.0, 0.0, 1.0, RED, Some([6.0, 4.0]));
        assert_eq!(short.vertex_count(), 6);
        let xs: Vec<f32> = short.view().chunks(STRIDE).map(|v| v[0]).collect();
        assert_eq!(xs[1], 6.0);

        let mut zero = Batch::new();
        zero.dashed_seg(0.0, 0.0, 4.0, 0.0, 1.0, RED, Some([0.0, 0.0]));
        assert_eq!(zero.vertex_count(), 6); // degenerate pattern draws solid
    }

    #[test]
    fn rect_stroke_is_four_segments() {
        let mut b = Batch::new();
        b.rect_stroke(0.0, 0.0, 10.0, 5.0, 1.0, RED);
        assert_eq!(b.vertex_count(), 24);
    }

    #[test]
    fn circle_emits_a_triangle_fan() {
        let mut b = Batch::new();
        b.circle(0.0, 0.0, 5.0, RED, 16);
        assert_eq!(b.vertex_count(), 16 * 3);
    }

    #[test]
    fn circle_feathers_its_rim() {
        let mut b = Batch::new();
        b.circle(10.0, 20.0, 3.0, RED, 16);
        let v = b.view();
        for i in 0..b.vertex_count() {
            let from_center = js_hypot(
                f64::from(v[i * STRIDE]) - 10.0,
                f64::from(v[i * STRIDE + 1]) - 20.0,
            );
            // edgeDist = distance from the center (float32)
            assert!((f64::from(v[i * STRIDE + 6]) - from_center).abs() < 1e-5);
            assert!((f64::from(v[i * STRIDE + 7]) - 3.0).abs() < 1e-9); // edgeHalf = the true radius
            let rounded = from_center.round();
            assert!(
                rounded == 0.0 || rounded == 4.0,
                "center, or rim padded by 1"
            );
        }
    }

    #[test]
    fn hypot_matches_the_obvious_values() {
        assert_eq!(js_hypot(3.0, 4.0), 5.0);
        assert_eq!(js_hypot(0.0, 0.0), 0.0);
        assert_eq!(js_hypot(-3.0, 0.0), 3.0);
        assert!(js_hypot(f64::NAN, 1.0).is_nan());
        assert_eq!(js_hypot(f64::NAN, f64::INFINITY), f64::INFINITY);
        assert_eq!(js_hypot(f64::INFINITY, 1.0), f64::INFINITY);
    }
    /// `(x, cos(x), sin(x))` as bit patterns, from Node's `Math.cos` / `Math.sin` (V8's
    /// fdlibm), over the angles of circle fans and a spread of arguments in `0..=7`.
    const NODE_TRIG: &[(u64, u64, u64)] = &[
        (0x3ff0c152382d7365, 0x3fe0000000000001, 0x3febb67ae8584caa),
        (0x4000c152382d7365, 0xbfdffffffffffffc, 0x3febb67ae8584cab),
        (0x400921fb54442d18, 0xbff0000000000000, 0x3ca1a62633145c07),
        (0x4010c152382d7365, 0xbfe0000000000004, 0xbfebb67ae8584ca9),
        (0x4014f1a6c638d03f, 0x3fe0000000000001, 0xbfebb67ae8584caa),
        (0x401921fb54442d18, 0x3ff0000000000000, 0xbcb1a62633145c07),
        (0x3fecb91f3bbba140, 0x3fe3f3a0e28bedd2, 0x3fe904c37505de4b),
        (0x3ffcb91f3bbba140, 0xbfcc7b90e3024580, 0x3fef329c0558e969),
        (0x40058ad76cccb8f0, 0xbfecd4bca9cb5c70, 0x3fdbc4c04d71abc3),
        (0x400cb91f3bbba140, 0xbfecd4bca9cb5c71, 0xbfdbc4c04d71abbf),
        (0x4011f3b3855544c8, 0xbfcc7b90e3024589, 0xbfef329c0558e969),
        (0x40158ad76cccb8f0, 0x3fe3f3a0e28bedd0, 0xbfe904c37505de4c),
        (0x401921fb54442d18, 0x3ff0000000000000, 0xbcb1a62633145c07),
        (0x3fe921fb54442d18, 0x3fe6a09e667f3bcd, 0x3fe6a09e667f3bcc),
        (0x3ff921fb54442d18, 0x3c91a62633145c07, 0x3ff0000000000000),
        (0x4002d97c7f3321d2, 0xbfe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
        (0x400921fb54442d18, 0xbff0000000000000, 0x3ca1a62633145c07),
        (0x400f6a7a2955385e, 0xbfe6a09e667f3bce, 0xbfe6a09e667f3bcc),
        (0x4012d97c7f3321d2, 0xbcaa79394c9e8a0a, 0xbff0000000000000),
        (0x4015fdbbe9bba775, 0x3fe6a09e667f3bcb, 0xbfe6a09e667f3bce),
        (0x401921fb54442d18, 0x3ff0000000000000, 0xbcb1a62633145c07),
        (0x3fd921fb54442d18, 0x3fed906bcf328d46, 0x3fd87de2a6aea963),
        (0x4002d97c7f3321d2, 0xbfe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
        (0x4011475cc9eedf00, 0xbfd87de2a6aea96d, 0xbfed906bcf328d44),
        (0x401921fb54442d18, 0x3ff0000000000000, 0xbcb1a62633145c07),
        (0x3fd41b2f769cf0e0, 0x3fee6f0e134454ff, 0x3fd3c6ef372fe94f),
        (0x3ffe28c731eb6950, 0xbfd3c6ef372fe94e, 0x3fee6f0e13445500),
        (0x400ba5614317cb35, 0xbfee6f0e134454ff, 0xbfd3c6ef372fe955),
        (0x40141b2f769cf0e0, 0x3fd3c6ef372fe94c, 0xbfee6f0e13445500),
        (0x3fc85f033a70a7d9, 0x3fef6bf5bb37e925, 0x3fc8396280a0ac90),
        (0x3ff247426bd47de3, 0x3fda9628d9c712b6, 0x3fed1bb48eee2c13),
        (0x4000c152382d7365, 0xbfdffffffffffffc, 0x3febb67ae8584cab),
        (0x40085f033a70a7d9, 0xbfefdae7eef1345e, 0x3fb85597c54753fc),
        (0x400ffcb43cb3dc4d, 0xbfe4f49e7f775888, 0xbfe82f19bb3a28a0),
        (0x4013cd329f7b8860, 0x3fce2d594ae05a74, 0xbfef191492fa5ee1),
        (0x40179c0b209d229b, 0x3fedb530aadeb15d, 0xbfd7c951530d93f5),
        (0x3fe0000000000000, 0x3fec1528065b7d50, 0x3fdeaee8744b05f0),
        (0x3ff0000000000000, 0x3fe14a280fb5068c, 0x3feaed548f090cee),
        (0x3ff8000000000000, 0x3fb21bd54fc5f9a7, 0x3fefeb7a9b2c6d8b),
        (0x4000000000000000, 0xbfdaa22657537205, 0x3fed18f6ead1b446),
        (0x4004000000000000, 0xbfe9a2f7ef858b7d, 0x3fe326af0dcfcab0),
        (0x4008000000000000, 0xbfefae04be85e5d2, 0x3fc210386db6d55b),
        (0x400c000000000000, 0xbfedf77403c11a5f, 0xbfd6733b7eba621f),
        (0x4010000000000000, 0xbfe4eaa606db24c1, 0xbfe837b9dddc1eae),
        (0x4012000000000000, 0xbfcafb5b54583d6a, 0xbfef47ed3dc74080),
        (0x4014000000000000, 0x3fd22785706b4ad9, 0xbfeeaf81f5e09933),
        (0x4016000000000000, 0x3fe6ad6c3c07d448, 0xbfe693c94e0ab057),
        (0x4018000000000000, 0x3feeb9b7097822f6, 0xbfd1e1f18ab0a2c0),
        (0x401a000000000000, 0x3fef4034b145c3c8, 0x3fcb890d40c6cb3c),
        (0x401c000000000000, 0x3fe81ff79ed92017, 0x3fe50608c26d0a08),
        (0x3ddb7cdfd9d7bdbb, 0x3ff0000000000000, 0x3ddb7cdfd9d7bdbb),
        (0x3fe921fb54442d18, 0x3fe6a09e667f3bcd, 0x3fe6a09e667f3bcc),
        (0x4002d97c7f3321d2, 0xbfe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
        (0x400f6a7a2955385e, 0xbfe6a09e667f3bce, 0xbfe6a09e667f3bcc),
    ];

    #[test]
    fn the_circle_trig_is_bit_exact_with_v8() {
        for (x, cos, sin) in NODE_TRIG {
            let a = f64::from_bits(*x);
            assert_eq!(fdlibm::cos(a).to_bits(), *cos, "cos({a})");
            assert_eq!(fdlibm::sin(a).to_bits(), *sin, "sin({a})");
        }
    }

    #[test]
    fn the_trig_outside_its_range_is_the_platforms() {
        assert_eq!(fdlibm::cos(-1.0).to_bits(), (-1.0f64).cos().to_bits());
        assert_eq!(fdlibm::sin(100.0).to_bits(), 100.0f64.sin().to_bits());
        assert!(fdlibm::cos(f64::NAN).is_nan());
    }
}
