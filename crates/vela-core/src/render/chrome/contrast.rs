use crate::render::backend::gl::color::{linearize, parse_color};

/// White or black text for a colored chip (a price tag, a timeline-mark token), biased
/// toward white so saturated brand colors (the default candle green and red sit at
/// L 0.22 to 0.24) read as white, while genuinely light colors (a white or pale candle
/// color) still get dark text. Uses relative luminance with a flip point of 0.4, higher
/// than `readable_text`'s WCAG crossover (about 0.18) which perceptually over-picks black
/// on mid-tone fills. Translucent `bg` is composited over `over` first so the choice
/// reflects what is actually seen.
pub fn tag_text_color(bg: &str, over: &str) -> &'static str {
    let [r, g, b, a] = parse_color(Some(bg));
    let (mut rr, mut gg, mut bb) = (r, g, b);
    if a < 1.0 {
        let [or, og, ob, _] = parse_color(Some(over));
        rr = r * a + or * (1.0 - a);
        gg = g * a + og * (1.0 - a);
        bb = b * a + ob * (1.0 - a);
    }
    let l = 0.2126 * linearize(rr) + 0.7152 * linearize(gg) + 0.0722 * linearize(bb);
    if l >= 0.4 { "#000000" } else { "#ffffff" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::backend::gl::color::readable_text;

    #[test]
    fn saturated_brand_colors_read_as_white() {
        assert_eq!(tag_text_color("#26a69a", "#000000"), "#ffffff");
        assert_eq!(tag_text_color("#ef5350", "#000000"), "#ffffff");
        assert_eq!(tag_text_color("#2962ff", "#000000"), "#ffffff");
    }

    #[test]
    fn light_colors_get_dark_text() {
        assert_eq!(tag_text_color("#ffffff", "#000000"), "#000000");
        assert_eq!(tag_text_color("#f5f5f5", "#000000"), "#000000");
        assert_eq!(tag_text_color("#ffeb3b", "#000000"), "#000000");
    }

    #[test]
    fn the_flip_point_is_biased_toward_white() {
        // Mid gray (L about 0.21) is white text here, where the WCAG pick is black.
        assert_eq!(tag_text_color("#808080", "#000000"), "#ffffff");
        assert_eq!(readable_text("#808080", "#000000"), "#000000");
    }

    #[test]
    fn a_translucent_color_is_composited_over_the_surface_first() {
        assert_eq!(
            tag_text_color("rgba(255,255,255,0.2)", "#000000"),
            "#ffffff"
        );
        assert_eq!(
            tag_text_color("rgba(255,255,255,0.2)", "#ffffff"),
            "#000000"
        );
    }

    #[test]
    fn an_unreadable_color_reads_as_black_so_the_text_is_white() {
        assert_eq!(tag_text_color("nonsense", "#000000"), "#ffffff");
    }
}
