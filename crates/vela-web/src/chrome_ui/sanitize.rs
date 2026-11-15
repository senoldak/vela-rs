//! A conservative HTML allowlist for host-supplied rich text (the timeline-mark popups): a
//! port of the original's `src/ui/sanitize-html.ts`.
//!
//! The policy is pure (`tag_disposition`, `attribute_allowed`, `safe_url`, `sanitize_nodes`)
//! and unit-tested natively. [`sanitize_html`] applies it to the browser's own parse of the
//! string (a `<template>` parses without executing anything) and rebuilds a fragment from
//! allowed elements and attributes only, so scripts, styles, frames, event handlers and
//! `javascript:` URLs never reach the page. Escaping would defeat the point (the markup
//! would show as source); filtering keeps the formatting and drops the hazards.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Document, DocumentFragment, Element, Node};

/// Elements kept as-is (with their allowed attributes).
const ALLOWED_TAGS: &[&str] = &[
    "a",
    "abbr",
    "b",
    "blockquote",
    "br",
    "code",
    "dd",
    "del",
    "div",
    "dl",
    "dt",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "mark",
    "ol",
    "p",
    "pre",
    "q",
    "s",
    "small",
    "span",
    "strong",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "u",
    "ul",
];

/// Elements dropped WITH their content: anything active, embedded, or form-shaped.
const DROPPED_TAGS: &[&str] = &[
    "script", "style", "iframe", "frame", "frameset", "object", "embed", "applet", "form", "input",
    "textarea", "button", "select", "option", "link", "meta", "base", "svg", "math", "template",
    "noscript", "audio", "video", "canvas", "dialog", "head", "title",
];

/// URL schemes that run code or smuggle content; a URL with any of these is dropped.
const BLOCKED_SCHEMES: &[&str] = &["javascript", "vbscript", "data", "file", "blob"];

/// What becomes of an element: kept, replaced by its children, or dropped with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Keep,
    Unwrap,
    Drop,
}

/// Whether an element survives (kept or unwrapped) or is dropped with its content.
pub fn tag_disposition(tag: &str) -> Disposition {
    let t = tag.to_lowercase();
    if DROPPED_TAGS.contains(&t.as_str()) {
        Disposition::Drop
    } else if ALLOWED_TAGS.contains(&t.as_str()) {
        Disposition::Keep
    } else {
        Disposition::Unwrap
    }
}

/// The attributes kept on `tag`, beyond the global `title`.
fn element_attributes(tag: &str) -> &'static [&'static str] {
    match tag {
        "a" => &["href"],
        "img" => &["src", "alt", "width", "height"],
        "td" | "th" => &["colspan", "rowspan"],
        "ol" => &["start"],
        _ => &[],
    }
}

/// Whether an attribute is kept on an element. Event handlers (`on*`) and `style` never are.
pub fn attribute_allowed(tag: &str, name: &str) -> bool {
    let n = name.to_lowercase();
    if n.starts_with("on") || n == "style" {
        return false;
    }
    if n == "title" {
        return true;
    }
    element_attributes(&tag.to_lowercase()).contains(&n.as_str())
}

/// A URL cleared for `href`/`src`, or `None`. Scheme-less (relative, `#anchor`) URLs pass;
/// `absolute_only` (image sources) additionally requires `http(s):`, so a page can never be
/// made to load an inline or local resource through a mark.
pub fn safe_url(value: &str, absolute_only: bool) -> Option<String> {
    // Control characters and whitespace are stripped first: a scheme split by a tab or a
    // newline (`java\tscript:`) still runs once the browser normalizes it.
    let url: String = value.chars().filter(|c| (*c as u32) > 32).collect();
    let scheme = url.split_once(':').and_then(|(head, _)| {
        let mut chars = head.chars();
        let first = chars.next()?;
        (first.is_ascii_alphabetic()
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
        .then(|| head.to_ascii_lowercase())
    });
    if let Some(s) = &scheme
        && BLOCKED_SCHEMES.contains(&s.as_str())
    {
        return None;
    }
    if absolute_only && !matches!(scheme.as_deref(), Some("http" | "https")) {
        return None;
    }
    Some(url)
}

/// A parsed node, as far as the policy cares: text, an element, or anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree {
    Text(String),
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
        children: Vec<Tree>,
    },
    /// Comments and processing instructions.
    Other,
}

/// Apply the policy to parsed nodes, returning the allowed subset. Links open in a new
/// browsing context with `rel="noopener noreferrer"`: a mark's link must never navigate the
/// chart's page nor reach back into it.
pub fn sanitize_nodes(nodes: &[Tree]) -> Vec<Tree> {
    let mut out = Vec::new();
    copy_children(nodes, &mut out);
    out
}

fn copy_children(from: &[Tree], to: &mut Vec<Tree>) {
    for child in from {
        match child {
            Tree::Text(t) => to.push(Tree::Text(t.clone())),
            Tree::Other => {}
            Tree::Element {
                tag,
                attrs,
                children,
            } => {
                let tag = tag.to_lowercase();
                match tag_disposition(&tag) {
                    Disposition::Drop => continue,
                    Disposition::Unwrap => {
                        copy_children(children, to);
                        continue;
                    }
                    Disposition::Keep => {}
                }
                let mut clean: Vec<(String, String)> = Vec::new();
                for (name, value) in attrs {
                    let name = name.to_lowercase();
                    if !attribute_allowed(&tag, &name) {
                        continue;
                    }
                    let mut value = value.clone();
                    if name == "href" || name == "src" {
                        match safe_url(&value, name == "src") {
                            Some(safe) => value = safe,
                            None => continue,
                        }
                    }
                    // `setAttribute` replaces: a repeated name keeps the last value.
                    match clean.iter_mut().find(|(n, _)| *n == name) {
                        Some(slot) => slot.1 = value,
                        None => clean.push((name, value)),
                    }
                }
                if tag == "a" {
                    for (name, value) in [("target", "_blank"), ("rel", "noopener noreferrer")] {
                        match clean.iter_mut().find(|(n, _)| n == name) {
                            Some(slot) => slot.1 = value.to_string(),
                            None => clean.push((name.to_string(), value.to_string())),
                        }
                    }
                }
                let mut kids = Vec::new();
                copy_children(children, &mut kids);
                to.push(Tree::Element {
                    tag,
                    attrs: clean,
                    children: kids,
                });
            }
        }
    }
}

const ELEMENT_NODE: u16 = 1;
const TEXT_NODE: u16 = 3;

fn read_nodes(parent: &Node) -> Vec<Tree> {
    let list = parent.child_nodes();
    let mut out = Vec::new();
    for i in 0..list.length() {
        let Some(node) = list.item(i) else { continue };
        match node.node_type() {
            TEXT_NODE => out.push(Tree::Text(node.text_content().unwrap_or_default())),
            ELEMENT_NODE => {
                let el: &Element = node.unchecked_ref();
                let named = el.attributes();
                let mut attrs = Vec::new();
                for j in 0..named.length() {
                    if let Some(a) = named.item(j) {
                        attrs.push((a.name(), a.value()));
                    }
                }
                out.push(Tree::Element {
                    tag: el.tag_name().to_lowercase(),
                    attrs,
                    children: read_nodes(&node),
                });
            }
            _ => out.push(Tree::Other),
        }
    }
    out
}

fn build_nodes(doc: &Document, nodes: &[Tree], into: &Node) {
    for n in nodes {
        match n {
            Tree::Text(t) => {
                let _ = into.append_child(&doc.create_text_node(t));
            }
            Tree::Element {
                tag,
                attrs,
                children,
            } => {
                let Ok(el) = doc.create_element(tag) else {
                    continue;
                };
                for (k, v) in attrs {
                    let _ = el.set_attribute(k, v);
                }
                build_nodes(doc, children, &el);
                let _ = into.append_child(&el);
            }
            Tree::Other => {}
        }
    }
}

/// Parse untrusted HTML and return a fragment carrying only the allowed subset.
pub fn sanitize_html(doc: &Document, html: &str) -> DocumentFragment {
    let out = doc.create_document_fragment();
    let Ok(tpl) = doc.create_element("template") else {
        return out;
    };
    tpl.set_inner_html(html);
    // `template.content` holds the inert parse; fall back to the element itself where a
    // stand-in DOM has no content fragment.
    let content = js_sys::Reflect::get(&tpl, &JsValue::from_str("content"))
        .ok()
        .filter(|c| c.is_object())
        .map(|c| c.unchecked_into::<Node>())
        .unwrap_or_else(|| tpl.unchecked_into::<Node>());
    let clean = sanitize_nodes(&read_nodes(&content));
    build_nodes(doc, &clean, &out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(tag: &str, attrs: &[(&str, &str)], children: Vec<Tree>) -> Tree {
        Tree::Element {
            tag: tag.to_string(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            children,
        }
    }
    fn text(t: &str) -> Tree {
        Tree::Text(t.to_string())
    }

    /// `innerHTML` of the sanitized nodes (text escaped as the DOM serializer does).
    fn render(nodes: &[Tree]) -> String {
        fn esc(s: &str) -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }
        fn go(n: &Tree, out: &mut String) {
            match n {
                Tree::Text(t) => out.push_str(&esc(t)),
                Tree::Other => {}
                Tree::Element {
                    tag,
                    attrs,
                    children,
                } => {
                    out.push('<');
                    out.push_str(tag);
                    for (k, v) in attrs {
                        out.push_str(&format!(" {k}=\"{}\"", v.replace('"', "&quot;")));
                    }
                    out.push('>');
                    if !matches!(tag.as_str(), "br" | "hr" | "img") {
                        for c in children {
                            go(c, out);
                        }
                        out.push_str(&format!("</{tag}>"));
                    }
                }
            }
        }
        let mut s = String::new();
        for n in sanitize_nodes(nodes) {
            go(&n, &mut s);
        }
        s
    }

    #[test]
    fn classifies_elements_formatting_kept_active_dropped_the_rest_unwrapped() {
        assert_eq!(tag_disposition("b"), Disposition::Keep);
        assert_eq!(tag_disposition("TABLE"), Disposition::Keep);
        assert_eq!(tag_disposition("script"), Disposition::Drop);
        assert_eq!(tag_disposition("iframe"), Disposition::Drop);
        assert_eq!(tag_disposition("svg"), Disposition::Drop);
        assert_eq!(tag_disposition("section"), Disposition::Unwrap);
        assert_eq!(tag_disposition("font"), Disposition::Unwrap);
    }

    #[test]
    fn keeps_only_the_per_element_attributes_never_handlers_or_styles() {
        assert!(attribute_allowed("a", "href"));
        assert!(!attribute_allowed("a", "onclick"));
        assert!(!attribute_allowed("a", "ONMOUSEOVER"));
        assert!(!attribute_allowed("p", "style"));
        assert!(attribute_allowed("p", "title"));
        assert!(attribute_allowed("img", "src"));
        assert!(!attribute_allowed("img", "srcset"));
        assert!(attribute_allowed("td", "colspan"));
        assert!(!attribute_allowed("div", "class"));
    }

    #[test]
    fn rejects_code_and_smuggling_schemes_whitespace_split_ones_included() {
        assert_eq!(
            safe_url("https://example.com/a", false).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            safe_url("/relative/path", false).as_deref(),
            Some("/relative/path")
        );
        assert_eq!(safe_url("#anchor", false).as_deref(), Some("#anchor"));
        assert_eq!(
            safe_url("mailto:a@b.c", false).as_deref(),
            Some("mailto:a@b.c")
        );
        assert_eq!(safe_url("javascript:alert(1)", false), None);
        assert_eq!(safe_url("JavaScript:alert(1)", false), None);
        assert_eq!(safe_url("java\tscript:alert(1)", false), None);
        assert_eq!(safe_url(" \n javascript:alert(1)", false), None);
        assert_eq!(safe_url("data:text/html,hi", false), None);
        assert_eq!(safe_url("vbscript:x", false), None);
    }

    #[test]
    fn image_sources_must_be_absolute_http() {
        assert_eq!(
            safe_url("https://cdn.example.com/x.png", true).as_deref(),
            Some("https://cdn.example.com/x.png")
        );
        assert_eq!(safe_url("/x.png", true), None);
        assert_eq!(safe_url("data:image/png;base64,AAAA", true), None);
    }

    #[test]
    fn keeps_formatting_and_text_drops_scripts_with_their_content() {
        let tree = vec![
            el("b", &[], vec![text("10-for-1 split")]),
            el("br", &[], vec![]),
            text("Effective "),
            el("i", &[], vec![text("10 Jun")]),
            el("script", &[], vec![text("alert(1)")]),
        ];
        assert_eq!(
            render(&tree),
            "<b>10-for-1 split</b><br>Effective <i>10 Jun</i>"
        );
        let framed = vec![el(
            "p",
            &[],
            vec![
                text("a"),
                el("iframe", &[("src", "https://x")], vec![]),
                text("b"),
            ],
        )];
        assert_eq!(render(&framed), "<p>ab</p>");
        let styled = vec![
            el("style", &[], vec![text("body{display:none}")]),
            text("text"),
        ];
        assert_eq!(render(&styled), "text");
    }

    #[test]
    fn strips_handlers_inline_styles_and_unsafe_urls_keeping_the_element() {
        assert_eq!(
            render(&[el("img", &[("src", "x"), ("onerror", "alert(1)")], vec![])]),
            "<img>"
        );
        assert_eq!(
            render(&[el(
                "p",
                &[("style", "color:red"), ("onclick", "x()"), ("title", "t")],
                vec![text("hi")]
            )]),
            "<p title=\"t\">hi</p>"
        );
        assert_eq!(
            render(&[el("a", &[("href", "javascript:alert(1)")], vec![text("x")])]),
            "<a target=\"_blank\" rel=\"noopener noreferrer\">x</a>"
        );
        assert_eq!(
            render(&[el(
                "img",
                &[("src", "data:image/png;base64,AAAA"), ("alt", "a")],
                vec![]
            )]),
            "<img alt=\"a\">"
        );
    }

    #[test]
    fn links_open_in_a_new_context_unknown_elements_unwrap_to_their_content() {
        assert_eq!(
            render(&[el(
                "a",
                &[("href", "https://example.com/f")],
                vec![text("Filing")]
            )]),
            "<a href=\"https://example.com/f\" target=\"_blank\" rel=\"noopener noreferrer\">Filing</a>"
        );
        assert_eq!(
            render(&[el(
                "section",
                &[],
                vec![el("font", &[("color", "red")], vec![text("plain")])]
            )]),
            "plain"
        );
        // The browser's parser inserts the implied <tbody>; the tree here carries it.
        let table = el(
            "table",
            &[],
            vec![el(
                "tbody",
                &[],
                vec![el(
                    "tr",
                    &[],
                    vec![el(
                        "td",
                        &[("colspan", "2"), ("bgcolor", "red")],
                        vec![text("c")],
                    )],
                )],
            )],
        );
        assert_eq!(
            render(&[table]),
            "<table><tbody><tr><td colspan=\"2\">c</td></tr></tbody></table>"
        );
    }

    #[test]
    fn text_stays_text_and_comments_vanish() {
        assert_eq!(
            render(&[text("1 < 2 & "), el("b", &[], vec![text("ok")])]),
            "1 &lt; 2 &amp; <b>ok</b>"
        );
        assert_eq!(render(&[Tree::Other, text("x")]), "x");
    }
}
