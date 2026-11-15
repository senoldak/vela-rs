//! Line breaking for text drawn on the canvas (a note, a box's label). Lines break at
//! spaces; scripts written without spaces between words (Thai, Lao, Khmer, Myanmar,
//! Chinese, Japanese) also break between their words, found by the `Intl` backend's word
//! segmenter.

use crate::i18n::text_segments;
use crate::intl::{Granularity, Intl};

use super::drawing_geometry::is_js_space;

/// Scripts that write words without spaces between them.
fn is_unspaced(c: char) -> bool {
    matches!(
        c as u32,
        0x0e00..=0x0eff   // Thai, Lao
            | 0x1000..=0x109f // Myanmar
            | 0x1780..=0x17ff // Khmer
            | 0x3040..=0x30ff // Hiragana, Katakana
            | 0x3400..=0x4dbf // CJK extension A
            | 0x4e00..=0x9fff // CJK unified ideographs
            | 0xf900..=0xfaff // CJK compatibility ideographs
    )
}

/// The break units of a run of text without spaces: its words, each carrying the
/// punctuation that follows it (a line never starts with a closing mark). One unit for
/// text in spaced scripts, or where the engine has no word segmenter.
fn break_units(intl: &dyn Intl, token: &str) -> Vec<String> {
    if !token.chars().any(is_unspaced) {
        return vec![token.to_string()];
    }
    let Some(words) = text_segments(intl, None, Granularity::Word, token) else {
        return vec![token.to_string()];
    };
    let mut units: Vec<String> = Vec::new();
    for seg in words {
        match units.last_mut() {
            Some(last) if seg.is_word_like == Some(false) => last.push_str(&seg.segment),
            _ => units.push(seg.segment),
        }
    }
    units
}

/// `para.split(/(\s+)/)`: the text and whitespace runs in order, whitespace kept, with
/// the empty strings JavaScript leaves at a leading or trailing edge.
fn split_keeping_whitespace(para: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space: Option<usize> = None;
    for (i, c) in para.char_indices() {
        match (is_js_space(c), in_space) {
            (true, None) => {
                out.push(&para[start..i]);
                in_space = Some(i);
            }
            (false, Some(from)) => {
                out.push(&para[from..i]);
                start = i;
                in_space = None;
            }
            _ => {}
        }
    }
    match in_space {
        Some(from) => {
            out.push(&para[from..]);
            out.push("");
        }
        None => out.push(&para[start..]),
    }
    out
}

/// Wrap `text` to lines no wider than `max_width` (as `measure` reports widths). Hard
/// line breaks are kept; a unit wider than the line on its own overflows on a line of its
/// own.
pub fn wrap_text_lines(
    intl: &dyn Intl,
    text: &str,
    max_width: f64,
    measure: &dyn Fn(&str) -> f64,
) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        if measure(para) <= max_width {
            out.push(para.to_string());
            continue;
        }
        let mut line = String::new();
        for token in split_keeping_whitespace(para) {
            let units = if token.chars().all(is_js_space) {
                vec![token.to_string()]
            } else {
                break_units(intl, token)
            };
            for unit in units {
                let trial = format!("{line}{unit}");
                if !line.is_empty() && measure(&trial) > max_width {
                    out.push(line.trim_end_matches(is_js_space).to_string());
                    line = unit.trim_start_matches(is_js_space).to_string();
                } else {
                    line = trial;
                }
            }
        }
        if !line.trim_matches(is_js_space).is_empty() {
            out.push(line.trim_end_matches(is_js_space).to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intl::{
        DateTimeFormatOptions, EnglishIntl, FormatPart, LocaleInfo, NumberFormatOptions,
        PluralCategory, PluralKind, Segment, TextDirection,
    };

    /// Ten pixels per character: easy widths to reason about.
    fn measure(text: &str) -> f64 {
        text.chars().count() as f64 * 10.0
    }

    /// The built-in backend, plus a Thai word list (the built-in one has no dictionary:
    /// the platform's `Intl.Segmenter` does).
    struct ThaiIntl;

    const THAI_WORDS: [&str; 7] = ["ราคา", "ปิด", "สูง", "กว่า", "ค่า", "เฉลี่ย", "เคลื่อนที่"];

    impl Intl for ThaiIntl {
        fn canonicalize_locale(&self, tag: &str) -> Option<String> {
            EnglishIntl.canonicalize_locale(tag)
        }
        fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
            EnglishIntl.parse_locale(tag)
        }
        fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
            EnglishIntl.maximize_locale(tag)
        }
        fn locale_text_direction(&self, tag: &str) -> Option<TextDirection> {
            EnglishIntl.locale_text_direction(tag)
        }
        fn locale_week_first_day(&self, tag: &str) -> Option<u32> {
            EnglishIntl.locale_week_first_day(tag)
        }
        fn plural_category(&self, l: &str, k: PluralKind, n: f64) -> Option<PluralCategory> {
            EnglishIntl.plural_category(l, k, n)
        }
        fn format_number(&self, l: &str, o: &NumberFormatOptions, v: f64) -> Option<String> {
            EnglishIntl.format_number(l, o, v)
        }
        fn format_number_to_parts(
            &self,
            l: &str,
            o: &NumberFormatOptions,
            v: f64,
        ) -> Option<Vec<FormatPart>> {
            EnglishIntl.format_number_to_parts(l, o, v)
        }
        fn resolved_numbering_system(&self, l: &str, r: Option<&str>) -> Option<String> {
            EnglishIntl.resolved_numbering_system(l, r)
        }
        fn format_date_time(&self, l: &str, o: &DateTimeFormatOptions, ms: f64) -> Option<String> {
            EnglishIntl.format_date_time(l, o, ms)
        }
        fn format_date_time_to_parts(
            &self,
            l: &str,
            o: &DateTimeFormatOptions,
            ms: f64,
        ) -> Option<Vec<FormatPart>> {
            EnglishIntl.format_date_time_to_parts(l, o, ms)
        }
        fn default_time_zone(&self) -> Option<String> {
            EnglishIntl.default_time_zone()
        }
        fn format_default_time(&self, ms: f64) -> String {
            EnglishIntl.format_default_time(ms)
        }
        fn segment(&self, _: Option<&str>, _: Granularity, text: &str) -> Option<Vec<Segment>> {
            let mut out = Vec::new();
            let mut rest = text;
            let mut index = 0;
            while !rest.is_empty() {
                let take = THAI_WORDS
                    .iter()
                    .filter(|w| rest.starts_with(**w))
                    .map(|w| w.len())
                    .max()
                    .unwrap_or_else(|| rest.chars().next().map_or(0, char::len_utf8));
                let (word, tail) = rest.split_at(take);
                out.push(Segment {
                    segment: word.to_string(),
                    index,
                    is_word_like: Some(true),
                });
                index += word.encode_utf16().count();
                rest = tail;
            }
            Some(out)
        }
        fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
            EnglishIntl.to_upper_case(text, locale)
        }
    }

    /// A backend with no segmenter at all.
    struct NoSegmenter;

    impl Intl for NoSegmenter {
        fn canonicalize_locale(&self, tag: &str) -> Option<String> {
            EnglishIntl.canonicalize_locale(tag)
        }
        fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
            EnglishIntl.parse_locale(tag)
        }
        fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
            EnglishIntl.maximize_locale(tag)
        }
        fn locale_text_direction(&self, tag: &str) -> Option<TextDirection> {
            EnglishIntl.locale_text_direction(tag)
        }
        fn locale_week_first_day(&self, tag: &str) -> Option<u32> {
            EnglishIntl.locale_week_first_day(tag)
        }
        fn plural_category(&self, l: &str, k: PluralKind, n: f64) -> Option<PluralCategory> {
            EnglishIntl.plural_category(l, k, n)
        }
        fn format_number(&self, l: &str, o: &NumberFormatOptions, v: f64) -> Option<String> {
            EnglishIntl.format_number(l, o, v)
        }
        fn format_number_to_parts(
            &self,
            l: &str,
            o: &NumberFormatOptions,
            v: f64,
        ) -> Option<Vec<FormatPart>> {
            EnglishIntl.format_number_to_parts(l, o, v)
        }
        fn resolved_numbering_system(&self, l: &str, r: Option<&str>) -> Option<String> {
            EnglishIntl.resolved_numbering_system(l, r)
        }
        fn format_date_time(&self, l: &str, o: &DateTimeFormatOptions, ms: f64) -> Option<String> {
            EnglishIntl.format_date_time(l, o, ms)
        }
        fn format_date_time_to_parts(
            &self,
            l: &str,
            o: &DateTimeFormatOptions,
            ms: f64,
        ) -> Option<Vec<FormatPart>> {
            EnglishIntl.format_date_time_to_parts(l, o, ms)
        }
        fn default_time_zone(&self) -> Option<String> {
            EnglishIntl.default_time_zone()
        }
        fn format_default_time(&self, ms: f64) -> String {
            EnglishIntl.format_default_time(ms)
        }
        fn segment(&self, _: Option<&str>, _: Granularity, _: &str) -> Option<Vec<Segment>> {
            None
        }
        fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
            EnglishIntl.to_upper_case(text, locale)
        }
    }

    #[test]
    fn thai_text_in_a_label_wraps_between_its_words_instead_of_running_off_the_box() {
        // "the close is above the moving average"
        let thai = "ราคาปิดสูงกว่าค่าเฉลี่ยเคลื่อนที่";
        let lines = wrap_text_lines(&ThaiIntl, thai, 100.0, &measure);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), thai);
        for line in &lines {
            assert!(measure(line) <= 100.0, "{line:?}");
        }
    }

    #[test]
    fn japanese_wraps_between_words_and_a_line_never_starts_with_a_full_stop() {
        let japanese = "価格が移動平均を上回りました。買いシグナルです。";
        let lines = wrap_text_lines(&EnglishIntl, japanese, 80.0, &measure);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), japanese);
        for line in &lines[1..] {
            assert!(!line.starts_with('。'));
        }
    }

    #[test]
    fn text_with_spaces_wraps_at_the_spaces_as_it_always_has() {
        let wrap = |t: &str, w: f64| wrap_text_lines(&EnglishIntl, t, w, &measure);
        assert_eq!(
            wrap("Close above the moving average", 120.0),
            ["Close above", "the moving", "average"]
        );
        assert_eq!(wrap("Short", 120.0), ["Short"]);
        assert_eq!(wrap("Two\nlines", 120.0), ["Two", "lines"]);
    }

    #[test]
    fn a_unit_wider_than_the_line_overflows_on_a_line_of_its_own() {
        let wrap = |t: &str| wrap_text_lines(&EnglishIntl, t, 50.0, &measure);
        assert_eq!(wrap("a extraordinarily b"), ["a", "extraordinarily", "b"]);
    }

    #[test]
    fn empty_and_blank_paragraphs_are_kept_as_hard_breaks() {
        let wrap = |t: &str| wrap_text_lines(&EnglishIntl, t, 50.0, &measure);
        assert_eq!(wrap(""), [""]);
        assert_eq!(wrap("a\n\nb"), ["a", "", "b"]);
    }

    #[test]
    fn whitespace_at_a_break_is_trimmed_and_an_overflowing_lead_leaves_an_empty_line() {
        // As in the original: the blank lead cannot fit beside the first word, so it is
        // flushed on its own (trimmed to empty).
        let wrap = |t: &str| wrap_text_lines(&EnglishIntl, t, 50.0, &measure);
        assert_eq!(wrap("  aaaa   bbbb  "), ["", "aaaa", "bbbb"]);
        assert_eq!(wrap("aaaa   bbbb"), ["aaaa", "bbbb"]);
    }

    #[test]
    fn without_a_segmenter_an_unspaced_run_stays_whole() {
        let japanese = "価格が移動平均を上回りました。";
        let lines = wrap_text_lines(&NoSegmenter, japanese, 80.0, &measure);
        assert_eq!(lines, [japanese]);
    }

    #[test]
    fn splitting_keeps_whitespace_runs_like_the_javascript_split() {
        assert_eq!(split_keeping_whitespace("a b"), ["a", " ", "b"]);
        assert_eq!(split_keeping_whitespace(" a"), ["", " ", "a"]);
        assert_eq!(split_keeping_whitespace("a "), ["a", " ", ""]);
        assert_eq!(split_keeping_whitespace("a  \t b"), ["a", "  \t ", "b"]);
        assert_eq!(split_keeping_whitespace("ab"), ["ab"]);
    }
}
