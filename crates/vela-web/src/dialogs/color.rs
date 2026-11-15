//! CSS colors for the native color controls.
//!
//! A swatch (`<input type="color">`) only holds `#rrggbb`, and the settings carry any CSS
//! color with an alpha, so a color control is a swatch plus an opacity slider: this module
//! reads a CSS color into channels and writes them back the way the chart stores them
//! (`#rrggbb` when opaque, `rgba(r, g, b, a)` otherwise).

/// 0-255 channels and an alpha in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f64,
}

fn hex_pair(hex: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()
}

fn hex_nibble(hex: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(hex.get(i..i + 1)?, 16)
        .ok()
        .map(|v| v * 17)
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()` and `rgba()`; anything else (a color
/// name, `transparent`, `currentColor`) is `None`.
pub fn parse(color: &str) -> Option<Rgba> {
    let s = color.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return match hex.len() {
            3 | 4 => Some(Rgba {
                r: hex_nibble(hex, 0)?,
                g: hex_nibble(hex, 1)?,
                b: hex_nibble(hex, 2)?,
                a: if hex.len() == 4 {
                    f64::from(hex_nibble(hex, 3)?) / 255.0
                } else {
                    1.0
                },
            }),
            6 | 8 => Some(Rgba {
                r: hex_pair(hex, 0)?,
                g: hex_pair(hex, 2)?,
                b: hex_pair(hex, 4)?,
                a: if hex.len() == 8 {
                    f64::from(hex_pair(hex, 6)?) / 255.0
                } else {
                    1.0
                },
            }),
            _ => None,
        };
    }
    let lower = s.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner
        .split([',', '/', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 3 || parts.len() > 4 {
        return None;
    }
    let channel = |p: &str| -> Option<u8> {
        let v: f64 = p.trim_end_matches('%').parse().ok()?;
        let v = if p.ends_with('%') { v * 2.55 } else { v };
        Some(v.round().clamp(0.0, 255.0) as u8)
    };
    let a = match parts.get(3) {
        Some(p) => {
            let v: f64 = p.trim_end_matches('%').parse().ok()?;
            (if p.ends_with('%') { v / 100.0 } else { v }).clamp(0.0, 1.0)
        }
        None => 1.0,
    };
    Some(Rgba {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
        a,
    })
}

/// The channels as `#rrggbb` (what a color input holds).
pub fn hex(c: &Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// The color the chart stores: `#rrggbb` while opaque, `rgba(r, g, b, a)` otherwise (alpha
/// rounded to two places).
pub fn css(c: &Rgba) -> String {
    let a = (c.a * 100.0).round() / 100.0;
    if a >= 1.0 {
        hex(c)
    } else {
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, trim_number(a))
    }
}

fn trim_number(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Whether a color reads as dark (the `color-scheme` of the dialogs).
pub fn is_dark(color: &str) -> bool {
    parse(color).is_none_or(|c| {
        0.299 * f64::from(c.r) + 0.587 * f64::from(c.g) + 0.114 * f64::from(c.b) < 140.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hex_and_functional_forms() {
        assert_eq!(
            parse("#3b82f6"),
            Some(Rgba {
                r: 59,
                g: 130,
                b: 246,
                a: 1.0
            })
        );
        assert_eq!(
            parse("#fff").map(|c| (c.r, c.g, c.b)),
            Some((255, 255, 255))
        );
        let c = parse("rgba(10, 20, 30, 0.5)").expect("parses");
        assert_eq!((c.r, c.g, c.b, c.a), (10, 20, 30, 0.5));
        assert!(parse("transparent").is_none());
        assert!(parse("#12").is_none());
    }

    #[test]
    fn writes_the_stored_forms() {
        let opaque = Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 1.0,
        };
        assert_eq!(css(&opaque), "#010203");
        let half = Rgba { a: 0.5, ..opaque };
        assert_eq!(css(&half), "rgba(1, 2, 3, 0.5)");
        let eighth = Rgba { a: 0.123, ..opaque };
        assert_eq!(css(&eighth), "rgba(1, 2, 3, 0.12)");
    }
}
