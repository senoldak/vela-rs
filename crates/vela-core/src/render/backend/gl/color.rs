//! CSS color string to straight (non-premultiplied) RGBA floats in `[0, 1]`.
//!
//! The model carries colors as CSS strings (hex, `rgb()`, `hsl()`, named). The GL
//! pipeline needs float RGBA per vertex. The original normalizes through a 1x1 canvas
//! read-back, which handles every CSS color form uniformly and caches each distinct
//! string. Core has no canvas, so this parses the forms itself and rounds the result the
//! way the read-back does (8-bit channels, premultiplied storage), so a color reads the
//! same in both. The cache is omitted: parsing a string is cheaper than the hashing a
//! cache would add, and it only existed to avoid the read-back.
//!
//! Forms handled: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()` / `rgba()` and
//! `hsl()` / `hsla()` (comma or space separated, numbers or percentages, an optional
//! `/ alpha`), the named colors and `transparent`. Any other string (an invalid color, or
//! a form such as `hwb()` and `color-mix()` that needs a full CSS engine) reads as opaque
//! black, which is what a canvas does when assigning it leaves the previous `#000`.

/// Straight (non-premultiplied) RGBA in `[0, 1]`.
pub type Rgba = [f64; 4];

const FALLBACK: Rgba = [0.0, 0.0, 0.0, 1.0];

/// The color as RGBA floats; opaque black for an empty or unreadable string.
pub fn parse_color(css: Option<&str>) -> Rgba {
    match css {
        Some(s) if !s.is_empty() => read_back(s),
        _ => FALLBACK,
    }
}

/// Black or white, whichever is easier to read on `bg`, chosen by WCAG contrast, so
/// chip and label text stays legible on any background (light or dark) with no
/// hardcoded assumption. A translucent `bg` is composited over `over` (the surface
/// behind it, e.g. the theme background) first, so the pick reflects what is actually
/// seen. Returns `"#000000"` or `"#ffffff"`.
pub fn readable_text(bg: &str, over: &str) -> &'static str {
    let [r, g, b, a] = parse_color(Some(bg));
    let (mut rr, mut gg, mut bb) = (r, g, b);
    if a < 1.0 {
        let [or, og, ob, _] = parse_color(Some(over));
        rr = r * a + or * (1.0 - a);
        gg = g * a + og * (1.0 - a);
        bb = b * a + ob * (1.0 - a);
    }
    let l = 0.2126 * linearize(rr) + 0.7152 * linearize(gg) + 0.0722 * linearize(bb);
    let contrast_black = (l + 0.05) / 0.05; // vs #000
    let contrast_white = 1.05 / (l + 0.05); // vs #fff
    if contrast_black >= contrast_white {
        "#000000"
    } else {
        "#ffffff"
    }
}

/// sRGB channel in `[0, 1]` to linear light, for a perceptual (relative luminance)
/// weighting.
pub fn linearize(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// `Math.round` for a non-negative number.
fn round_half_up(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// What a 1x1 canvas read-back yields for a parsed color: 8-bit alpha, channels stored
/// premultiplied and un-premultiplied on the way out.
fn read_back(css: &str) -> Rgba {
    let Some((r, g, b, a)) = parse_css(css.trim()) else {
        return FALLBACK;
    };
    let a8 = round_half_up(a.clamp(0.0, 1.0) * 255.0);
    let c8 = |c: f64| round_half_up(c.clamp(0.0, 255.0));
    let (r8, g8, b8) = (c8(r), c8(g), c8(b));
    if a8 == 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let unpremultiply = |c: f64| {
        if a8 == 255.0 {
            c
        } else {
            round_half_up(round_half_up(c * a8 / 255.0) * 255.0 / a8).min(255.0)
        }
    };
    [
        unpremultiply(r8) / 255.0,
        unpremultiply(g8) / 255.0,
        unpremultiply(b8) / 255.0,
        a8 / 255.0,
    ]
}

/// `(r, g, b)` in `0..=255` (unrounded) and alpha in `0..=1`.
type Parsed = (f64, f64, f64, f64);

fn parse_css(s: &str) -> Option<Parsed> {
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = s.to_ascii_lowercase();
    if let Some(open) = lower.find('(') {
        let inner = lower[open + 1..].strip_suffix(')')?;
        return match lower[..open].trim() {
            "rgb" | "rgba" => parse_rgb(inner),
            "hsl" | "hsla" => parse_hsl(inner),
            _ => None,
        };
    }
    if lower == "transparent" {
        return Some((0.0, 0.0, 0.0, 0.0));
    }
    named_color(&lower).map(|(r, g, b)| (f64::from(r), f64::from(g), f64::from(b), 1.0))
}

fn parse_hex(hex: &str) -> Option<Parsed> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digits: Vec<u32> = hex.chars().filter_map(|c| c.to_digit(16)).collect();
    let channel = |i: usize| -> f64 { f64::from(digits[i] * 16 + digits[i + 1]) };
    let short = |i: usize| -> f64 { f64::from(digits[i] * 17) };
    match digits.len() {
        3 => Some((short(0), short(1), short(2), 1.0)),
        4 => Some((short(0), short(1), short(2), short(3) / 255.0)),
        6 => Some((channel(0), channel(2), channel(4), 1.0)),
        8 => Some((channel(0), channel(2), channel(4), channel(6) / 255.0)),
        _ => None,
    }
}

/// The components of a functional notation: separated by commas or whitespace, with an
/// optional `/` before the alpha.
fn components(inner: &str) -> Vec<&str> {
    inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect()
}

/// A number, or a percentage of `full` (`50%` of 255 is 127.5).
fn number_or_percent(token: &str, full: f64) -> Option<f64> {
    match token.strip_suffix('%') {
        Some(p) => p.parse::<f64>().ok().map(|v| v / 100.0 * full),
        None => token.parse::<f64>().ok(),
    }
    .filter(|v| v.is_finite())
}

fn alpha_of(token: Option<&&str>) -> Option<f64> {
    match token {
        None => Some(1.0),
        Some(t) => number_or_percent(t, 1.0).map(|a| a.clamp(0.0, 1.0)),
    }
}

fn parse_rgb(inner: &str) -> Option<Parsed> {
    let p = components(inner);
    if p.len() != 3 && p.len() != 4 {
        return None;
    }
    Some((
        number_or_percent(p[0], 255.0)?,
        number_or_percent(p[1], 255.0)?,
        number_or_percent(p[2], 255.0)?,
        alpha_of(p.get(3))?,
    ))
}

fn parse_hsl(inner: &str) -> Option<Parsed> {
    let p = components(inner);
    if p.len() != 3 && p.len() != 4 {
        return None;
    }
    let hue_token = p[0].strip_suffix("deg").unwrap_or(p[0]);
    let h = hue_token.parse::<f64>().ok().filter(|v| v.is_finite())?;
    let s = (number_or_percent(p[1], 1.0)?).clamp(0.0, 1.0);
    let l = (number_or_percent(p[2], 1.0)?).clamp(0.0, 1.0);
    let h = h.rem_euclid(360.0) / 360.0;
    let t2 = if l <= 0.5 {
        l * (s + 1.0)
    } else {
        l + s - l * s
    };
    let t1 = l * 2.0 - t2;
    let channel = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t * 6.0 < 1.0 {
            t1 + (t2 - t1) * t * 6.0
        } else if t * 2.0 < 1.0 {
            t2
        } else if t * 3.0 < 2.0 {
            t1 + (t2 - t1) * (2.0 / 3.0 - t) * 6.0
        } else {
            t1
        };
        v * 255.0
    };
    Some((
        channel(h + 1.0 / 3.0),
        channel(h),
        channel(h - 1.0 / 3.0),
        alpha_of(p.get(3))?,
    ))
}

/// The CSS named colors (lowercase), as `(r, g, b)`.
fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    NAMED
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| {
            let rgb = NAMED[i].1;
            ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
        })
}

/// Sorted by name for binary search.
const NAMED: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Rgba {
        parse_color(Some(s))
    }

    #[test]
    fn the_named_table_is_sorted_and_complete() {
        assert!(NAMED.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(NAMED.len(), 148);
    }

    #[test]
    fn hex_forms() {
        assert_eq!(p("#ff0000"), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p("#fff"), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(p("#00000000"), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            p("#4caf50"),
            [76.0 / 255.0, 175.0 / 255.0, 80.0 / 255.0, 1.0]
        );
        assert_eq!(p("#f008")[3], 136.0 / 255.0);
        assert_eq!(p("#ff000080")[3], 128.0 / 255.0);
    }

    #[test]
    fn functional_forms() {
        assert_eq!(p("rgb(255, 0, 0)"), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p("rgba(0,0,255,1)"), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(p("rgb(0 255 0 / 50%)")[3], 128.0 / 255.0);
        assert_eq!(p("rgb(100%, 0%, 0%)"), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p("hsl(120, 100%, 50%)"), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(p("hsla(0, 100%, 50%, 1)"), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p("RGB(255,0,0)"), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn translucent_colors_read_back_through_premultiplied_storage() {
        // Full channels survive a half-alpha round trip exactly.
        let [r, g, b, a] = p("rgba(255, 0, 0, 0.5)");
        assert_eq!([r, g, b], [1.0, 0.0, 0.0]);
        assert_eq!(a, 128.0 / 255.0);
        // A transparent color loses its channels.
        assert_eq!(p("rgba(10, 20, 30, 0)"), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(p("transparent"), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn named_colors_and_case() {
        assert_eq!(p("white"), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(
            p("RebeccaPurple"),
            [102.0 / 255.0, 51.0 / 255.0, 153.0 / 255.0, 1.0]
        );
        assert_eq!(p("  navy "), [0.0, 0.0, 128.0 / 255.0, 1.0]);
    }

    #[test]
    fn unreadable_strings_fall_back_to_opaque_black() {
        for s in [
            "",
            "not-a-color",
            "#12",
            "#ggg",
            "rgb(1,2)",
            "hwb(0 0% 0%)",
            "rgb(a,b,c)",
            "#",
        ] {
            assert_eq!(p(s), FALLBACK, "{s:?}");
        }
        assert_eq!(parse_color(None), FALLBACK);
        assert_eq!(parse_color(Some("")), FALLBACK);
    }

    #[test]
    fn readable_text_picks_the_higher_contrast_of_black_and_white() {
        assert_eq!(readable_text("#ffffff", "#000000"), "#000000");
        assert_eq!(readable_text("#000000", "#000000"), "#ffffff");
        assert_eq!(readable_text("#2962ff", "#000000"), "#ffffff");
        assert_eq!(readable_text("#ffeb3b", "#000000"), "#000000");
    }

    #[test]
    fn readable_text_composites_a_translucent_background_over_the_surface() {
        // Half-transparent white over black is mid gray: white reads better on it.
        assert_eq!(readable_text("rgba(255,255,255,0.2)", "#000000"), "#ffffff");
        // Over a white surface the same fill is near white: black reads better.
        assert_eq!(readable_text("rgba(255,255,255,0.2)", "#ffffff"), "#000000");
    }

    #[test]
    fn linearize_matches_the_srgb_curve() {
        assert_eq!(linearize(0.0), 0.0);
        assert!((linearize(1.0) - 1.0).abs() < 1e-12);
        assert!((linearize(0.5) - 0.214_041_14).abs() < 1e-7);
        assert!((linearize(0.04) - 0.04 / 12.92).abs() < 1e-12);
    }
}
