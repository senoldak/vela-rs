//! The design tokens (`--vela-*` custom properties) the shell chrome reads.
//!
//! The original writes them with `ensureUIHost(root, theme)` from `@luxalgo/vela-ui`
//! (`themeTokens` in `lib/tokens.ts`): the static scale (spacing, radii, z-index, motion,
//! type) and the theme tokens derived from the chart theme's colors. The port has no
//! `vela-ui` package, so this module is that writer: the same names and the same formulas,
//! written as inline custom properties on the widget root. Every shell component reads
//! `var(--vela-bg)`, `var(--vela-border-soft)`, ... and follows a theme change because
//! [`apply_theme`] rewrites them in place.

use vela_core::ports::chart_renderer::VelaTheme;
use vela_core::util::js_number;
use web_sys::Element;

use crate::dom::set_style;

const ACCENT: &str = "#2962ff";
const ACCENT_BRIGHT: &str = "#38c0fd";
const HIGHLIGHT: &str = "#e0b400";

/// The theme-independent tokens.
pub const STATIC_TOKENS: &[(&str, &str)] = &[
    ("--vela-space-1", "4px"),
    ("--vela-space-2", "8px"),
    ("--vela-space-3", "12px"),
    ("--vela-space-4", "16px"),
    ("--vela-radius-sm", "4px"),
    ("--vela-radius-md", "6px"),
    ("--vela-radius-lg", "10px"),
    ("--vela-z-tooltip", "60"),
    ("--vela-z-menu", "50"),
    ("--vela-z-dialog", "40"),
    ("--vela-z-popover", "6000"),
    ("--vela-ease", "cubic-bezier(0.22, 1, 0.36, 1)"),
    ("--vela-dur-fast", "90ms"),
    ("--vela-dur-med", "160ms"),
    ("--vela-font-size-sm", "11px"),
    ("--vela-font-size-md", "12px"),
    ("--vela-font-size-lg", "14px"),
    ("--vela-scroll-size", "5px"),
];

#[derive(Clone, Copy)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

fn parse_hex(h: &str) -> Option<Rgb> {
    let byte = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    if !h.is_ascii() {
        return None;
    }
    match h.len() {
        3 => {
            let d = |i: usize| byte(&h[i..=i].repeat(2));
            Some(Rgb {
                r: d(0)?,
                g: d(1)?,
                b: d(2)?,
            })
        }
        6 | 8 => Some(Rgb {
            r: byte(&h[0..2])?,
            g: byte(&h[2..4])?,
            b: byte(&h[4..6])?,
        }),
        _ => None,
    }
}

/// `#RGB`, `#RRGGBB`, `#RRGGBBAA`, `rgb()` and `rgba()` into 0-255 channels.
fn parse_rgb(color: &str) -> Option<Rgb> {
    let s = color.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = s.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?;
    let mut parts = inner.trim_end_matches(')').split(',');
    let mut next = || parts.next()?.trim().parse::<f64>().ok();
    Some(Rgb {
        r: next()?,
        g: next()?,
        b: next()?,
    })
}

/// `rgba(r,g,b,alpha)`; an unparseable color is returned unchanged.
pub fn with_alpha(color: &str, alpha: f64) -> String {
    match parse_rgb(color) {
        Some(c) => format!(
            "rgba({},{},{},{})",
            js_number(c.r),
            js_number(c.g),
            js_number(c.b),
            js_number(alpha)
        ),
        None => color.to_string(),
    }
}

/// `top` at `alpha` flattened over `base` into an opaque color.
pub fn mix(base: &str, top: &str, alpha: f64) -> String {
    let (Some(b), Some(t)) = (parse_rgb(base), parse_rgb(top)) else {
        return base.to_string();
    };
    let c = |x: f64, y: f64| (x + (y - x) * alpha).round();
    format!(
        "rgb({},{},{})",
        js_number(c(b.r, t.r)),
        js_number(c(b.g, t.g)),
        js_number(c(b.b, t.b))
    )
}

/// Perceived-dark check; an unparseable color counts as dark.
pub fn is_dark_color(color: &str) -> bool {
    match parse_rgb(color) {
        Some(c) => 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 128.0,
        None => true,
    }
}

fn color_of(theme: &VelaTheme, key: &str, fallback: &str) -> String {
    theme
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(fallback)
        .to_string()
}

/// Every theme token for one chart theme, as `(--vela-name, value)` (`themeTokens`).
pub fn theme_tokens(theme: &VelaTheme) -> Vec<(&'static str, String)> {
    let background = color_of(theme, "background", "#151619");
    let text = color_of(theme, "textColor", "#b2b5be");
    let border = color_of(theme, "borderColor", "#2a2b30");
    let up = color_of(theme, "upColor", "#089981");
    let down = color_of(theme, "downColor", "#f23645");
    let font = color_of(theme, "fontFamily", "sans-serif");
    let dark = is_dark_color(&background);
    let wash = |a: f64| {
        if dark {
            format!("rgba(255,255,255,{})", js_number(a))
        } else {
            with_alpha(&text, a + 0.02)
        }
    };
    let elevated = mix(
        &background,
        if dark { "#ffffff" } else { &text },
        if dark { 0.03 } else { 0.05 },
    );
    let pick = |d: &str, l: String| if dark { d.to_string() } else { l };
    vec![
        ("--vela-font", font),
        ("--vela-bg", background.clone()),
        ("--vela-fg", pick("#d1d4dc", text.clone())),
        ("--vela-fg-muted", pick("#868a96", with_alpha(&text, 0.62))),
        ("--vela-fg-faint", with_alpha(&text, 0.35)),
        ("--vela-fg-bright", pick("#f0f3fa", "#000000".to_string())),
        ("--vela-surface", background.clone()),
        ("--vela-surface-elev", elevated.clone()),
        ("--vela-surface-overlay", elevated),
        ("--vela-surface-sunken", background.clone()),
        ("--vela-border", border.clone()),
        (
            "--vela-border-strong",
            pick("#34353b", with_alpha(&text, 0.28)),
        ),
        ("--vela-border-soft", border.clone()),
        ("--vela-border-faint", with_alpha(&text, 0.08)),
        ("--vela-hover", wash(0.06)),
        ("--vela-active", wash(0.1)),
        ("--vela-hover-strong", wash(0.16)),
        ("--vela-focus", with_alpha(&text, 0.5)),
        ("--vela-focus-soft", with_alpha(&text, 0.12)),
        ("--vela-separator-hover-band", with_alpha(&text, 0.1)),
        ("--vela-separator-hover-line", with_alpha(&text, 0.55)),
        ("--vela-scroll", border),
        ("--vela-accent", ACCENT.to_string()),
        ("--vela-accent-bright", ACCENT_BRIGHT.to_string()),
        ("--vela-highlight", HIGHLIGHT.to_string()),
        ("--vela-selected-bg", pick("#f0f3fa", text.clone())),
        (
            "--vela-selected-fg",
            pick(&background, "#ffffff".to_string()),
        ),
        ("--vela-fg-on-fill", "#ffffff".to_string()),
        ("--vela-up", up.clone()),
        ("--vela-down", down.clone()),
        ("--vela-profit", up),
        ("--vela-loss", down.clone()),
        ("--vela-danger", down),
        ("--vela-shadow", "0 8px 30px rgba(0,0,0,0.5)".to_string()),
        (
            "--vela-shadow-dialog",
            "0 20px 60px rgba(0,0,0,0.5)".to_string(),
        ),
        ("--vela-backdrop", "rgba(0,0,0,0.45)".to_string()),
    ]
}

/// Write the static tokens and `theme`'s tokens on `el` (inline custom properties).
pub fn apply_theme(el: &Element, theme: &VelaTheme) {
    for (name, value) in STATIC_TOKENS {
        set_style(el, name, value);
    }
    for (name, value) in theme_tokens(theme) {
        set_style(el, name, &value);
    }
}

/// The chart theme's seam color: the grid gap and the screenshot's gap fill.
pub fn seam_color(theme: &VelaTheme) -> String {
    color_of(theme, "borderColor", "#2a2b30")
}

/// Whether `theme`'s surface is dark (the ink of the attribution mark follows it).
pub fn is_dark(theme: &VelaTheme) -> bool {
    is_dark_color(&color_of(theme, "background", "#151619"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vela_core::theme::{dark_theme, light_theme};

    fn get(list: &[(&'static str, String)], name: &str) -> String {
        list.iter().find(|(n, _)| *n == name).unwrap().1.clone()
    }

    #[test]
    fn the_dark_tokens_follow_the_original_formulas() {
        let t = theme_tokens(&dark_theme());
        assert_eq!(get(&t, "--vela-bg"), "#151619");
        assert_eq!(get(&t, "--vela-fg-bright"), "#f0f3fa");
        assert_eq!(get(&t, "--vela-hover"), "rgba(255,255,255,0.06)");
        assert_eq!(get(&t, "--vela-border-soft"), "#2a2b30");
        assert_eq!(
            get(&t, "--vela-separator-hover-band"),
            "rgba(178,181,190,0.1)"
        );
    }

    #[test]
    fn the_light_tokens_derive_from_the_text_color() {
        let t = theme_tokens(&light_theme());
        assert_eq!(get(&t, "--vela-fg-bright"), "#000000");
        assert_eq!(get(&t, "--vela-fg"), "#1e293b");
        assert!(!is_dark(&light_theme()) && is_dark(&dark_theme()));
    }
}
