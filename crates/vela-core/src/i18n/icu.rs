//! A dependency-free subset of ICU MessageFormat: `{name}`, `{n, number}` (with the
//! `integer` and `percent` styles), `{n, plural, ...}` and `{n, selectordinal, ...}` with
//! exact `=N` matches, `offset:` and `#`, and `{x, select, ...}`. Apostrophes quote the
//! way ICU's default mode does: `''` is one apostrophe, `'{...}'` is literal text, and any
//! other apostrophe is plain text (so `l'indicateur` needs no escaping).
//!
//! Parsing never panics: malformed input is a [`SyntaxError`], and the translator falls
//! back to the English text. Parsed messages are not cached here; a caller that formats
//! the same message often may keep the [`parse_message`] result.

use std::collections::HashMap;
use std::fmt;

use crate::format::numbers::format_message_number;
use crate::format::types::FormatSettings;
use crate::intl::{PluralCategory, PluralKind};

use super::number_text;

/// A parsed message.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(String),
    Arg {
        name: String,
        /// `{n, number}`: format a numeric value (and numeric text) as a number.
        number: bool,
        /// The `number` style (`integer`, `percent`).
        style: Option<String>,
    },
    Plural {
        name: String,
        ordinal: bool,
        offset: f64,
        options: HashMap<String, Vec<Node>>,
    },
    Select {
        name: String,
        options: HashMap<String, Vec<Node>>,
    },
    /// `#` inside a plural branch: the (offset-adjusted) count.
    Pound,
}

/// A value a message interpolates.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageValue {
    Text(String),
    Number(f64),
}

/// Values a message interpolates, by argument name.
pub type MessageValues = HashMap<String, MessageValue>;

/// A malformed message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyntaxError {}

/// JavaScript's `\s` (and `String#trim`) whitespace.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `Number(text)`: a decimal, hex (`0x`), octal (`0o`) or binary (`0b`) literal or
/// `Infinity`, trimmed; 0 for blank text, `NaN` for anything else.
pub(crate) fn js_to_number(text: &str) -> f64 {
    let t = text.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    let radix = match t.as_bytes() {
        [b'0', b'x' | b'X', ..] => Some(16),
        [b'0', b'o' | b'O', ..] => Some(8),
        [b'0', b'b' | b'B', ..] => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        let digits = &t[2..];
        if digits.is_empty() {
            return f64::NAN;
        }
        let mut value = 0.0f64;
        for c in digits.chars() {
            match c.to_digit(radix) {
                Some(d) => value = value * f64::from(radix) + f64::from(d),
                None => return f64::NAN,
            }
        }
        return value;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if is_decimal_literal(t) {
        t.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn is_decimal_literal(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let int_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let frac_start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        frac_digits = i - frac_start;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp_start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == b.len()
}

struct Parser {
    src: Vec<char>,
    i: usize,
}

const MAX_DEPTH: usize = 8;

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            src: source.chars().collect(),
            i: 0,
        }
    }

    fn parse(&mut self) -> Result<Vec<Node>, SyntaxError> {
        let nodes = self.message(false, 0)?;
        if self.i < self.src.len() {
            return Err(self.error("unmatched }"));
        }
        Ok(nodes)
    }

    fn error(&self, what: &str) -> SyntaxError {
        // Positions and the quoted source read as JavaScript's UTF-16 string would.
        let at: usize = self.src[..self.i.min(self.src.len())]
            .iter()
            .map(|c| c.len_utf16())
            .sum();
        let source: String = self.src.iter().collect();
        SyntaxError {
            message: format!("[vela] message syntax: {what} at {at} in \"{source}\""),
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.src.get(self.i + offset).copied()
    }

    /// Text and arguments up to an unmatched `}` (left unread) or the end.
    fn message(&mut self, in_plural: bool, depth: usize) -> Result<Vec<Node>, SyntaxError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        let mut out: Vec<Node> = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, out: &mut Vec<Node>| {
            if !text.is_empty() {
                out.push(Node::Text(std::mem::take(text)));
            }
        };
        while let Some(ch) = self.peek(0) {
            match ch {
                '\'' => {
                    let next = self.peek(1);
                    if next == Some('\'') {
                        text.push('\'');
                        self.i += 2;
                    } else if matches!(next, Some('{' | '}')) || (in_plural && next == Some('#')) {
                        // Quoted literal: runs to the next lone apostrophe ('' inside is one).
                        self.i += 1;
                        while let Some(c) = self.peek(0) {
                            if c == '\'' {
                                if self.peek(1) == Some('\'') {
                                    text.push('\'');
                                    self.i += 2;
                                    continue;
                                }
                                self.i += 1;
                                break;
                            }
                            text.push(c);
                            self.i += 1;
                        }
                    } else {
                        text.push('\'');
                        self.i += 1;
                    }
                }
                '{' => {
                    flush(&mut text, &mut out);
                    self.i += 1;
                    out.push(self.argument(depth)?);
                }
                '}' => break,
                '#' if in_plural => {
                    flush(&mut text, &mut out);
                    out.push(Node::Pound);
                    self.i += 1;
                }
                other => {
                    text.push(other);
                    self.i += 1;
                }
            }
        }
        flush(&mut text, &mut out);
        Ok(out)
    }

    fn skip_space(&mut self) {
        while self.peek(0).is_some_and(is_js_space) {
            self.i += 1;
        }
    }

    fn word(&mut self) -> String {
        self.skip_space();
        let start = self.i;
        while self
            .peek(0)
            .is_some_and(|c| !is_js_space(c) && !matches!(c, ',' | '{' | '}'))
        {
            self.i += 1;
        }
        self.src[start..self.i].iter().collect()
    }

    fn expect(&mut self, ch: char) -> Result<(), SyntaxError> {
        self.skip_space();
        if self.peek(0) != Some(ch) {
            return Err(self.error(&format!("expected \"{ch}\"")));
        }
        self.i += 1;
        Ok(())
    }

    fn starts_with(&self, prefix: &str) -> bool {
        prefix
            .chars()
            .enumerate()
            .all(|(k, c)| self.peek(k) == Some(c))
    }

    /// After a `{`: the argument through its closing `}`.
    fn argument(&mut self, depth: usize) -> Result<Node, SyntaxError> {
        let name = self.word();
        if name.is_empty() {
            return Err(self.error("empty argument name"));
        }
        self.skip_space();
        if self.peek(0) == Some('}') {
            self.i += 1;
            return Ok(Node::Arg {
                name,
                number: false,
                style: None,
            });
        }
        self.expect(',')?;
        let kind = self.word();
        self.skip_space();
        if kind == "number" {
            let mut style = None;
            if self.peek(0) == Some(',') {
                self.i += 1;
                style = Some(self.word()).filter(|s| !s.is_empty());
            }
            self.expect('}')?;
            return Ok(Node::Arg {
                name,
                number: true,
                style,
            });
        }
        if matches!(kind.as_str(), "plural" | "selectordinal" | "select") {
            self.expect(',')?;
            let mut offset = 0.0;
            self.skip_space();
            if kind != "select" && self.starts_with("offset:") {
                self.i += "offset:".len();
                offset = js_to_number(&self.word());
                if !offset.is_finite() {
                    return Err(self.error("bad offset"));
                }
            }
            let mut options: HashMap<String, Vec<Node>> = HashMap::new();
            loop {
                self.skip_space();
                if self.peek(0) == Some('}') {
                    self.i += 1;
                    break;
                }
                let key = self.word();
                if key.is_empty() {
                    return Err(self.error("expected a selector"));
                }
                self.expect('{')?;
                let branch = self.message(kind != "select", depth + 1)?;
                options.insert(key, branch);
                self.expect('}')?;
            }
            if !options.contains_key("other") {
                return Err(self.error(&format!("\"{name}\" has no \"other\" option")));
            }
            return Ok(if kind == "select" {
                Node::Select { name, options }
            } else {
                Node::Plural {
                    name,
                    ordinal: kind == "selectordinal",
                    offset,
                    options,
                }
            });
        }
        Err(self.error(&format!("unknown argument type \"{kind}\"")))
    }
}

/// Parse a message. A [`SyntaxError`] on malformed input.
pub fn parse_message(source: &str) -> Result<Vec<Node>, SyntaxError> {
    Parser::new(source).parse()
}

/// The plural category of `n` for the settings' locale; an unusable locale reads English
/// rules, as the original's `catch` does.
fn plural_category(settings: &FormatSettings, ordinal: bool, n: f64) -> PluralCategory {
    let kind = if ordinal {
        PluralKind::Ordinal
    } else {
        PluralKind::Cardinal
    };
    settings
        .intl
        .plural_category(&settings.locale, kind, n)
        .or_else(|| settings.intl.plural_category("en", kind, n))
        .unwrap_or(PluralCategory::Other)
}

/// The text a value selects with: JavaScript's `String(raw)`, `"undefined"` when unbound.
fn selector_text(raw: Option<&MessageValue>) -> String {
    match raw {
        None => "undefined".to_string(),
        Some(MessageValue::Text(t)) => t.clone(),
        Some(MessageValue::Number(n)) => number_text(*n),
    }
}

fn render(
    nodes: &[Node],
    values: Option<&MessageValues>,
    settings: &FormatSettings,
    pound: Option<f64>,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Pound => match pound {
                Some(p) => out.push_str(&format_message_number(p, settings, None)),
                None => out.push('#'),
            },
            Node::Arg {
                name,
                number,
                style,
            } => {
                let style = if *number { style.as_deref() } else { None };
                match values.and_then(|v| v.get(name)) {
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                    Some(MessageValue::Number(n)) => {
                        out.push_str(&format_message_number(*n, settings, style))
                    }
                    Some(MessageValue::Text(t)) => {
                        let as_number = js_to_number(t);
                        if *number
                            && !t.trim_matches(is_js_space).is_empty()
                            && as_number.is_finite()
                        {
                            out.push_str(&format_message_number(as_number, settings, style));
                        } else {
                            out.push_str(t);
                        }
                    }
                }
            }
            Node::Select { name, options } => {
                let raw = values.and_then(|v| v.get(name));
                let branch = options
                    .get(&selector_text(raw))
                    .or_else(|| options.get("other"));
                if let Some(branch) = branch {
                    render(branch, values, settings, pound, out);
                }
            }
            Node::Plural {
                name,
                ordinal,
                offset,
                options,
            } => {
                let n = match values.and_then(|v| v.get(name)) {
                    Some(MessageValue::Number(n)) => *n,
                    Some(MessageValue::Text(t)) => js_to_number(t),
                    None => f64::NAN,
                };
                let mut branch: Option<&Vec<Node>> = None;
                if n.is_finite() {
                    branch = options.get(&format!("={}", number_text(n))).or_else(|| {
                        options.get(plural_category(settings, *ordinal, n - offset).as_str())
                    });
                }
                if let Some(branch) = branch.or_else(|| options.get("other")) {
                    render(
                        branch,
                        values,
                        settings,
                        n.is_finite().then_some(n - offset),
                        out,
                    );
                }
            }
        }
    }
}

/// Format a message with its values. A [`SyntaxError`] on malformed input.
pub fn format_message(
    source: &str,
    values: Option<&MessageValues>,
    settings: &FormatSettings,
) -> Result<String, SyntaxError> {
    // Fast path: nothing to parse.
    if !source.contains('{') && !source.contains('\'') {
        return Ok(source.to_string());
    }
    let nodes = parse_message(source)?;
    let mut out = String::with_capacity(source.len());
    render(&nodes, values, settings, None, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en() -> FormatSettings {
        FormatSettings::english()
    }

    fn fmt(source: &str, values: &[(&str, MessageValue)]) -> String {
        let values: MessageValues = values
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        format_message(source, Some(&values), &en()).unwrap_or_else(|e| format!("ERR {e}"))
    }

    fn n(v: f64) -> MessageValue {
        MessageValue::Number(v)
    }

    fn t(v: &str) -> MessageValue {
        MessageValue::Text(v.to_string())
    }

    #[test]
    fn plain_text_takes_the_fast_path() {
        assert_eq!(fmt("no arguments", &[]), "no arguments");
        assert_eq!(fmt("", &[]), "");
    }

    #[test]
    fn plural_nested_in_select_and_pound() {
        let m = "{who, select, ana {{n, plural, one {Ana has # bar} other {Ana has # bars}}} other {Someone: {n, plural, one {# bar} other {# bars}}}}";
        assert_eq!(fmt(m, &[("who", t("ana")), ("n", n(1.0))]), "Ana has 1 bar");
        assert_eq!(
            fmt(m, &[("who", t("ana")), ("n", n(1200.0))]),
            "Ana has 1,200 bars"
        );
        assert_eq!(
            fmt(m, &[("who", t("bob")), ("n", n(2.0))]),
            "Someone: 2 bars"
        );
    }

    #[test]
    fn pound_is_literal_outside_a_plural_and_inside_a_select_branch() {
        assert_eq!(fmt("Band #{n}", &[("n", n(2.0))]), "Band #2");
        let m = "{n, plural, other {{k, select, a {# is literal} other {x}}}}";
        assert_eq!(fmt(m, &[("n", n(3.0)), ("k", t("a"))]), "# is literal");
    }

    #[test]
    fn exact_matches_and_offsets_win_over_categories() {
        let m = "{n, plural, offset:1 =0 {nobody} =1 {only {who}} one {{who} and # other} other {{who} and # others}}";
        let who = ("who", t("Ana"));
        assert_eq!(fmt(m, &[("n", n(0.0)), who.clone()]), "nobody");
        assert_eq!(fmt(m, &[("n", n(1.0)), who.clone()]), "only Ana");
        assert_eq!(fmt(m, &[("n", n(2.0)), who.clone()]), "Ana and 1 other");
        assert_eq!(fmt(m, &[("n", n(5.0)), who]), "Ana and 4 others");
    }

    #[test]
    fn escaped_apostrophes() {
        assert_eq!(
            fmt("l'indicateur {name}", &[("name", t("RSI"))]),
            "l'indicateur RSI"
        );
        assert_eq!(
            fmt("'{name}' n''est pas {name}", &[("name", t("RSI"))]),
            "{name} n'est pas RSI"
        );
        assert_eq!(fmt("it''s", &[]), "it's");
        assert_eq!(fmt("'{a''b}'", &[]), "{a'b}");
        assert_eq!(
            fmt("{n, plural, other {'#' stays, # counts}}", &[("n", n(3.0))]),
            "# stays, 3 counts"
        );
        // An unterminated quote runs to the end.
        assert_eq!(fmt("a '{b", &[]), "a {b");
    }

    #[test]
    fn malformed_messages_are_errors_not_panics() {
        for bad in [
            "{count, plural, one {# bar",
            "{count",
            "{",
            "}",
            "a } b",
            "{}",
            "{n, plural, one {x}}",
            "{n, wat, other {x}}",
            "{n, plural offset:1}",
            "{n, plural, offset:x other {x}}",
            "{n, select, }",
            "{n, plural, {x}}",
            "{n number}",
        ] {
            assert!(parse_message(bad).is_err(), "{bad}");
        }
        let deep = format!("{}x{}", "{n, select, other {".repeat(10), "}}".repeat(10));
        assert!(parse_message(&deep).is_err());
        let ok = format!("{}x{}", "{n, select, other {".repeat(8), "}}".repeat(8));
        assert!(parse_message(&ok).is_ok());
    }

    #[test]
    fn error_text_names_the_position_and_the_source() {
        let err = parse_message("ab {n, plural, one {x}}")
            .err()
            .map(|e| e.message)
            .unwrap_or_default();
        assert!(err.contains("has no \"other\" option"), "{err}");
        assert!(err.contains("in \"ab {n, plural, one {x}}\""), "{err}");
    }

    #[test]
    fn unknown_arguments_stay_visible() {
        assert_eq!(fmt("Hello {name}", &[]), "Hello {name}");
        assert_eq!(
            fmt(
                "{n, number} of {total, number, integer}",
                &[("n", n(1234.5)), ("total", n(9876.5))]
            ),
            "1,234.5 of 9,877"
        );
        // An unbound count is no number: the `other` branch, with a literal `#`.
        assert_eq!(
            fmt("{count, plural, one {# bar} other {# bars}}", &[]),
            "# bars"
        );
    }

    #[test]
    fn missing_other_is_a_syntax_error() {
        assert!(parse_message("{n, plural, one {a} few {b}}").is_err());
        assert!(parse_message("{s, select, a {a}}").is_err());
    }

    #[test]
    fn select_falls_back_to_other_and_matches_numbers_by_text() {
        let m = "{side, select, buy {Bought} sell {Sold} other {Traded}}";
        assert_eq!(fmt(m, &[("side", t("buy"))]), "Bought");
        assert_eq!(fmt(m, &[("side", t("short"))]), "Traded");
        assert_eq!(fmt(m, &[]), "Traded");
        assert_eq!(
            fmt("{n, select, 2 {two} other {many}}", &[("n", n(2.0))]),
            "two"
        );
    }

    #[test]
    fn numeric_text_formats_as_a_number_only_for_number_arguments() {
        assert_eq!(fmt("{x, number}", &[("x", t("1234.5"))]), "1,234.5");
        assert_eq!(fmt("{x}", &[("x", t("1234.5"))]), "1234.5");
        assert_eq!(fmt("{x, number}", &[("x", t("abc"))]), "abc");
        assert_eq!(fmt("{x, number}", &[("x", t("  "))]), "  ");
        assert_eq!(fmt("{x, number, percent}", &[("x", n(0.25))]), "25%");
    }

    #[test]
    fn plural_text_values_read_as_javascript_numbers() {
        let m = "{n, plural, =0 {none} one {one} other {#}}";
        assert_eq!(fmt(m, &[("n", t(""))]), "none");
        assert_eq!(fmt(m, &[("n", t("0x10"))]), "16");
        assert_eq!(fmt(m, &[("n", t("abc"))]), "#");
    }

    #[test]
    fn ordinals_pick_their_own_categories() {
        let m = "{n, selectordinal, one {#st} two {#nd} few {#rd} other {#th}}";
        let got: Vec<String> = [1.0, 2.0, 3.0, 4.0, 11.0, 21.0, 22.0]
            .iter()
            .map(|v| fmt(m, &[("n", n(*v))]))
            .collect();
        assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "21st", "22nd"]);
    }

    #[test]
    fn js_number_parsing() {
        assert_eq!(js_to_number(" 12 "), 12.0);
        assert_eq!(js_to_number(""), 0.0);
        assert_eq!(js_to_number("1e3"), 1000.0);
        assert_eq!(js_to_number(".5"), 0.5);
        assert_eq!(js_to_number("5."), 5.0);
        assert_eq!(js_to_number("0b101"), 5.0);
        assert_eq!(js_to_number("-Infinity"), f64::NEG_INFINITY);
        for bad in ["inf", "nan", "1_000", "1 2", "0x", "--1", "e5", "+", "."] {
            assert!(js_to_number(bad).is_nan(), "{bad}");
        }
    }
}
