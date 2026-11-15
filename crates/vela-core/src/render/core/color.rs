//! The kit's color helpers the renderer's configuration needs (the original
//! imports them from `@luxalgo/vela-ui/color`): parsing a CSS color to
//! channels, and `with_alpha`, which yields an `rgba(...)` string. This is not
//! [`crate::indicators::classics::shared::with_alpha`], which writes
//! `#rrggbbaa` for indicator inks.

use crate::util::js_number;

/// 0-255 channels (fractional when the source was an `rgb()` string).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// Parse `#RGB` / `#RRGGBB` / `#RRGGBBAA` / `rgb()` / `rgba()` into channels.
pub fn parse_rgb(color: &str) -> Option<Rgb> {
    let s = color.trim_matches(is_js_whitespace);
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f64::from);
        return match hex.len() {
            3 => {
                let nibble = |i: usize| {
                    u8::from_str_radix(&hex[i..i + 1], 16)
                        .ok()
                        .map(|v| f64::from(v * 17))
                };
                Some(Rgb {
                    r: nibble(0)?,
                    g: nibble(1)?,
                    b: nibble(2)?,
                })
            }
            6..=8 => Some(Rgb {
                r: pair(0)?,
                g: pair(2)?,
                b: pair(4)?,
            }),
            _ => None,
        };
    }
    parse_functional(s)
}

/// `rgb(` / `rgba(` followed by three numbers (the alpha, if any, is ignored).
fn parse_functional(s: &str) -> Option<Rgb> {
    let lower = s.get(..5).map(str::to_ascii_lowercase);
    let rest = if s.get(..4).map(str::to_ascii_lowercase).as_deref() == Some("rgb(") {
        &s[4..]
    } else if lower.as_deref() == Some("rgba(") {
        &s[5..]
    } else {
        return None;
    };
    let mut rest = rest.trim_start_matches(is_js_whitespace);
    let mut channels = [0.0; 3];
    for (i, slot) in channels.iter_mut().enumerate() {
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        *slot = parse_float_prefix(&rest[..end]);
        rest = rest[end..].trim_start_matches(is_js_whitespace);
        if i < 2 {
            rest = rest.strip_prefix(',')?.trim_start_matches(is_js_whitespace);
        }
    }
    Some(Rgb {
        r: channels[0],
        g: channels[1],
        b: channels[2],
    })
}

/// `parseFloat` of a run of digits and dots: the longest valid numeric prefix.
fn parse_float_prefix(run: &str) -> f64 {
    let mut seen_dot = false;
    let mut end = 0;
    for (i, c) in run.char_indices() {
        if c == '.' {
            if seen_dot {
                break;
            }
            seen_dot = true;
        }
        end = i + 1;
    }
    run[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// Whether a character is whitespace or a line terminator to `String.prototype.trim`.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Apply an alpha to any parseable color, giving an `rgba(...)` string.
/// Unparseable input is returned unchanged so callers can pass through
/// `transparent`, `currentColor`, etc.
pub fn with_alpha(color: &str, alpha: f64) -> String {
    match parse_rgb(color) {
        None => color.to_string(),
        Some(c) => format!(
            "rgba({},{},{},{})",
            js_number(c.r),
            js_number(c.g),
            js_number(c.b),
            js_number(alpha)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_hex_and_functional_forms() {
        let rgb = |r, g, b| Some(Rgb { r, g, b });
        assert_eq!(parse_rgb("#fff"), rgb(255.0, 255.0, 255.0));
        assert_eq!(parse_rgb(" #3B82F6 "), rgb(59.0, 130.0, 246.0));
        assert_eq!(parse_rgb("#3b82f680"), rgb(59.0, 130.0, 246.0));
        assert_eq!(parse_rgb("rgb(1, 2, 3)"), rgb(1.0, 2.0, 3.0));
        assert_eq!(parse_rgb("RGBA(10,20,30,0.5)"), rgb(10.0, 20.0, 30.0));
        assert_eq!(parse_rgb("rgb(1.5,2,3)"), rgb(1.5, 2.0, 3.0));
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "red",
            "#12",
            "#12345",
            "#zzzzzz",
            "rgb(1,2)",
            "rgb(a,b,c)",
            "#",
        ] {
            assert_eq!(parse_rgb(bad), None, "{bad}");
        }
    }

    #[test]
    fn with_alpha_writes_an_rgba_string() {
        assert_eq!(with_alpha("#3b82f6", 0.28), "rgba(59,130,246,0.28)");
        assert_eq!(with_alpha("#089981", 0.25), "rgba(8,153,129,0.25)");
        assert_eq!(with_alpha("#ff9800", 0.08), "rgba(255,152,0,0.08)");
        assert_eq!(with_alpha("rgb(1,2,3)", 1.0), "rgba(1,2,3,1)");
    }

    #[test]
    fn with_alpha_passes_unparseable_colors_through() {
        assert_eq!(with_alpha("transparent", 0.5), "transparent");
        assert_eq!(with_alpha("", 0.5), "");
    }

    #[test]
    fn non_ascii_input_does_not_panic() {
        assert_eq!(parse_rgb("rgb(é"), None);
        assert_eq!(parse_rgb("#é"), None);
        assert_eq!(parse_rgb("€€€€€€€€"), None);
    }
}
