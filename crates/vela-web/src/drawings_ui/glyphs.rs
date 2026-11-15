//! The glyphs of the settings popup and panel.
//!
//! The registry glyphs the toolbar already has live in [`crate::icons`]; the ones only the
//! popup uses are copied here from the original's icon registry
//! (`packages/vela-ui/src/lib/icons.ts`), and the parameterised ones (a line at a width and
//! dash, a stamp at a size) are generated like the original's `DrawingSettingsPopup.ts` does.

/// Tier A (16x16, stroke 1.2) chrome icon.
fn svg16(body: &str, extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("{extra} ")
    };
    format!(
        "<svg viewBox=\"0 0 16 16\" width=\"1em\" height=\"1em\" {extra}fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{body}</svg>"
    )
}

/// Tier B (24x24, stroke 1.8) drawing-tool icon.
pub fn svg24(body: &str, extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("{extra} ")
    };
    format!(
        "<svg viewBox=\"0 0 24 24\" width=\"1em\" height=\"1em\" {extra}fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{body}</svg>"
    )
}

/// A tier-B glyph drawn as a solid shape.
pub fn svg24_solid(body: &str) -> String {
    format!(
        "<svg viewBox=\"0 0 24 24\" width=\"1em\" height=\"1em\" fill=\"currentColor\" stroke=\"none\">{body}</svg>"
    )
}

/// The registry glyph `id` (empty for an unknown one): the toolbar's set first, then the
/// popup's own.
pub fn glyph(id: &str) -> String {
    let shared = crate::icons::icon(id);
    if !shared.is_empty() {
        return shared.to_string();
    }
    match id {
        "brush" => svg24("<path d=\"M9.5 12 17 4.5a2.12 2.12 0 0 1 3 3L12.5 15\"/><path d=\"M7 14a3 3 0 0 0-3 3c0 1.3-1.2 1.5-1.5 2 .8.9 2 1.5 3.5 1.5a3.5 3.5 0 0 0 3.5-3.5 3 3 0 0 0-2.5-3Z\"/>", ""),
        "bucket" => svg24("<path d=\"m18.5 11.5-7-7L4 12a1.8 1.8 0 0 0 0 2.5l5 5a1.8 1.8 0 0 0 2.5 0Z\"/><path d=\"m5 5 4 4\"/><path d=\"M3.5 13.5h13\"/><path d=\"M21 17.5c0 1.1-.9 2-2 2s-2-.9-2-2c0-1 1.2-1.7 2-3 .8 1.3 2 2 2 3Z\"/>", ""),
        "type" => svg24("<path d=\"M5 6V4.5h14V6\"/><path d=\"M12 4.5v15\"/><path d=\"M9.5 19.5h5\"/>", ""),
        "bold" => svg24("<path d=\"M7 5h6a3.5 3.5 0 0 1 0 7H7Z\"/><path d=\"M7 12h7a3.5 3.5 0 0 1 0 7H7Z\"/>", "stroke-width=\"2.4\""),
        "italic" => svg24("<path d=\"M15 5h-5M14 19H9M14.5 5 10 19\"/>", "stroke-width=\"2.2\""),
        "price-delta" => svg24("<path d=\"M12 4v16\"/><path d=\"M8 8l4-4 4 4\"/><path d=\"M8 16l4 4 4-4\"/>", ""),
        "date-delta" => svg24("<path d=\"M4 12h16\"/><path d=\"M8 8l-4 4 4 4\"/><path d=\"M16 8l4 4-4 4\"/>", ""),
        "r-squared" => "<svg viewBox=\"0 0 24 24\" width=\"1em\" height=\"1em\" fill=\"currentColor\" stroke=\"none\"><text x=\"2.5\" y=\"17.5\" font-size=\"14\" font-family=\"serif\">R\u{b2}</text></svg>".to_string(),
        "bands" => svg24("<path d=\"M3 6h18\"/><path d=\"M3 18h18\"/><path d=\"M3 12h18\" stroke-dasharray=\"3 3\"/>", ""),
        "dedekind" => svg24("<path d=\"M2 20h20\"/><path d=\"M4 20a8 8 0 0 1 16 0\"/><path d=\"M8 20a4 4 0 0 1 8 0\"/><path d=\"M12 4v16\"/>", ""),
        "sonic" => svg24("<circle cx=\"15\" cy=\"12\" r=\"4\"/><circle cx=\"12\" cy=\"12\" r=\"2.5\"/><path d=\"M8 5v14\"/>", ""),
        "supersonic" => svg24("<circle cx=\"16\" cy=\"12\" r=\"3.5\"/><path d=\"M6 12 15 6M6 12 15 18\"/>", ""),
        "grip" => svg16("<circle cx=\"6\" cy=\"3.5\" r=\"1\"/><circle cx=\"10\" cy=\"3.5\" r=\"1\"/><circle cx=\"6\" cy=\"8\" r=\"1\"/><circle cx=\"10\" cy=\"8\" r=\"1\"/><circle cx=\"6\" cy=\"12.5\" r=\"1\"/><circle cx=\"10\" cy=\"12.5\" r=\"1\"/>", "fill=\"currentColor\" stroke=\"none\""),
        "kebab" => svg16("<circle cx=\"8\" cy=\"3.2\" r=\"1.2\"/><circle cx=\"8\" cy=\"8\" r=\"1.2\"/><circle cx=\"8\" cy=\"12.8\" r=\"1.2\"/>", "fill=\"currentColor\" stroke=\"none\""),
        "gear" => svg16("<path d=\"M6.55 1.4h2.9l.32 1.72c.48.14.92.38 1.3.7l1.66-.55 1.45 2.5-1.35 1.1c.08.34.12.7.12 1.07s-.04.73-.12 1.07l1.35 1.1-1.45 2.5-1.66-.55a4.3 4.3 0 0 1-1.3.7L9.45 14.6h-2.9l-.32-1.72a4.3 4.3 0 0 1-1.3-.7l-1.66.55-1.45-2.5 1.35-1.1A4.4 4.4 0 0 1 3.05 7.94c0-.37.04-.73.12-1.07l-1.35-1.1 1.45-2.5 1.66.55c.38-.32.82-.56 1.3-.7L6.55 1.4z\"/><circle cx=\"8\" cy=\"8\" r=\"2.2\"/>", ""),
        "chevron-down" => svg16("<path d=\"M3.5 6 8 10.5 12.5 6\"/>", ""),
        "reset" => svg16("<path d=\"M2 8a6 6 0 1 0 6-6 6.5 6.5 0 0 0-4.5 1.83L2 5.33\"/><path d=\"M2 2v3.33h3.33\"/>", ""),
        _ => String::new(),
    }
}

/// Force an inline SVG to `size` px (the original's `sized`): the new attributes come
/// first, and HTML keeps the first of a duplicated one.
pub fn sized(svg: &str, size: u32) -> String {
    svg.replacen(
        "<svg ",
        &format!("<svg width=\"{size}\" height=\"{size}\" "),
        1,
    )
}

/// The popup panel's own small glyphs (12-16 px, `currentColor`).
pub const CHEVRON: &str = "<svg viewBox=\"0 0 12 12\" width=\"12\" height=\"12\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"m4.5 2.5 3.5 3.5-3.5 3.5\"/></svg>";
pub const CHEVRON_DOWN: &str = "<svg viewBox=\"0 0 12 12\" width=\"12\" height=\"12\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"m2.5 4.5 3.5 3.5 3.5-3.5\"/></svg>";
pub const CROSS: &str = "<svg viewBox=\"0 0 16 16\" width=\"14\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\"><path d=\"m4.5 4.5 7 7M11.5 4.5l-7 7\"/></svg>";

/// The extend segment's icons (`none`, `left`, `right`, `both`).
pub fn extend_icon(kind: &str) -> &'static str {
    match kind {
        "left" => {
            "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M1 7h12\"/><path d=\"m3.5 4.5-2.5 2.5 2.5 2.5\"/><circle cx=\"13\" cy=\"7\" r=\"1.5\" fill=\"currentColor\"/></svg>"
        }
        "right" => {
            "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M5 7h12\"/><path d=\"m14.5 4.5 2.5 2.5-2.5 2.5\"/><circle cx=\"5\" cy=\"7\" r=\"1.5\" fill=\"currentColor\"/></svg>"
        }
        "both" => {
            "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M1 7h16\"/><path d=\"m3.5 4.5-2.5 2.5 2.5 2.5M14.5 4.5l2.5 2.5-2.5 2.5\"/></svg>"
        }
        _ => {
            "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M5 7h8\"/><circle cx=\"5\" cy=\"7\" r=\"1.5\" fill=\"currentColor\"/><circle cx=\"13\" cy=\"7\" r=\"1.5\" fill=\"currentColor\"/></svg>"
        }
    }
}

/// The panel's width segment glyph: a bar `w` px thick.
pub fn width_icon(w: f64) -> String {
    format!(
        "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\"><rect x=\"2\" y=\"{}\" width=\"14\" height=\"{w}\" rx=\"{}\" fill=\"currentColor\"/></svg>",
        7.0 - w / 2.0,
        w / 2.0
    )
}

/// The panel's dash segment glyph.
pub fn dash_icon(style: &str) -> String {
    let dash = match style {
        "dashed" => "4 3",
        "dotted" => "0.1 3.5",
        _ => "0",
    };
    format!(
        "<svg viewBox=\"0 0 18 14\" width=\"18\" height=\"14\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-dasharray=\"{dash}\"><path d=\"M2 7h14\"/></svg>"
    )
}

/// A line glyph at a width and style, for the bar's width / style dropdowns.
pub fn line_icon(width: f64, style: &str) -> String {
    let dash = match style {
        "dashed" => "5,3",
        "dotted" => "1.5,3",
        _ => "",
    };
    let width = if width.is_finite() && width != 0.0 {
        width
    } else {
        2.0
    };
    svg24(
        &format!(
            "<line x1=\"3.5\" y1=\"12\" x2=\"20.5\" y2=\"12\" stroke-width=\"{width}\" stroke-dasharray=\"{dash}\"/>"
        ),
        "",
    )
}

/// A centered text glyph on the tier-B grid: the base of the generated previews.
fn text_glyph(text: &str, font_size: f64, y: f64, extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!(" {extra}")
    };
    svg24_solid(&format!(
        "<text x=\"12\" y=\"{y}\" font-size=\"{font_size}\" text-anchor=\"middle\"{extra}>{text}</text>"
    ))
}

/// The font px each named size renders the icon glyph at.
fn size_px(size: &str) -> f64 {
    match size {
        "small" => 10.0,
        "large" => 16.0,
        "huge" => 20.0,
        _ => 13.0,
    }
}

/// The current stamp, for the icon dropdown's trigger. The glyph text is escaped.
pub fn glyph_icon(g: &str) -> String {
    text_glyph(&escape(g), 15.0, 17.0, "")
}

/// A dot that grows with the chosen stamp size.
pub fn stamp_size_icon(size: &str) -> String {
    text_glyph("\u{25cf}", size_px(size) + 4.0, 17.0, "")
}

/// A letter (S / M / L / H) for the text size dropdown of the text panel.
pub fn size_icon(size: &str) -> String {
    let letter: String = size
        .chars()
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    text_glyph(&escape(&letter), 15.0, 17.0, "")
}

/// A "12" that grows with the chosen size (fib level numbers).
pub fn numbers_size_icon(size: &str) -> String {
    text_glyph("12", size_px(size) - 1.0, 16.5, "font-weight=\"600\"")
}

/// A "T" that grows with the chosen size (fib level labels, text size).
pub fn label_size_icon(size: &str) -> String {
    text_glyph("T", size_px(size) + 2.0, 17.0, "")
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sized_puts_the_new_size_first() {
        let s = sized(&glyph("brush"), 17);
        assert!(s.starts_with("<svg width=\"17\" height=\"17\" viewBox"));
    }

    #[test]
    fn every_glyph_the_popup_uses_exists() {
        for id in [
            "brush",
            "bucket",
            "type",
            "price-delta",
            "date-delta",
            "lock",
            "unlock",
            "bring-front",
            "send-back",
            "trash",
            "bold",
            "italic",
            "grip",
            "kebab",
            "clone",
            "reset",
            "gear",
            "chevron-down",
            "r-squared",
            "bands",
            "dedekind",
            "sonic",
            "supersonic",
        ] {
            assert!(glyph(id).starts_with("<svg "), "{id}");
        }
        assert_eq!(glyph("nope"), "");
    }

    #[test]
    fn generated_glyphs_follow_their_value() {
        assert!(line_icon(3.0, "dashed").contains("stroke-width=\"3\""));
        assert!(line_icon(3.0, "dashed").contains("stroke-dasharray=\"5,3\""));
        assert!(line_icon(0.0, "solid").contains("stroke-width=\"2\""));
        assert!(stamp_size_icon("huge").contains("font-size=\"24\""));
        assert!(numbers_size_icon("small").contains("font-size=\"9\""));
        assert!(glyph_icon("<").contains("&lt;"));
        assert!(dash_icon("dotted").contains("0.1 3.5"));
    }
}
