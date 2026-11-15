//! The icons of the legend, the pane controls, the scroll button and the menus, copied from
//! the original's registry (`packages/vela-ui/src/lib/icons.ts`, tier A: 16 grid, stroke 1.2).
//! Every icon strokes `currentColor`. The registry of the original is page-wide and open to
//! `registerIcon`; here [`register`] adds ids to this crate's table (a legend action or callout
//! names its icon by id) and [`markup`] reads either.

use std::cell::RefCell;
use std::collections::HashMap;

fn s16(body: &str) -> String {
    format!(
        "<svg viewBox=\"0 0 16 16\" width=\"1em\" height=\"1em\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{body}</svg>"
    )
}

thread_local! {
    static REGISTERED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Register (or replace) an icon's raw `<svg>` markup under an id.
pub fn register(id: &str, svg: &str) {
    REGISTERED.with(|r| {
        r.borrow_mut().insert(id.to_string(), svg.to_string());
    });
}

fn builtin(id: &str) -> Option<String> {
    let body = match id {
        "chevron-down" => r#"<path d="M3.5 6 8 10.5 12.5 6"/>"#,
        "chevron-up" => r#"<path d="M3.5 10 8 5.5 12.5 10"/>"#,
        "chevrons-right" => {
            r#"<path d="m4 3.5 4.5 4.5L4 12.5"/><path d="m8.5 3.5 4.5 4.5-4.5 4.5"/>"#
        }
        "close" => r#"<path d="m3.8 3.8 8.4 8.4M12.2 3.8l-8.4 8.4"/>"#,
        "check" => r#"<path d="m2.8 8.4 3.4 3.4 7-7.6"/>"#,
        "eye" => {
            r#"<path d="M1.5 8s2.5-5.4 6.5-5.4S14.5 8 14.5 8 12 13.4 8 13.4 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="1.8"/>"#
        }
        "eye-off" => {
            r#"<path d="M1.5 8s2.5-5.4 6.5-5.4S14.5 8 14.5 8 12 13.4 8 13.4 1.5 8 1.5 8z" opacity="0.45"/><path d="m3 13 10-10"/>"#
        }
        "gear" => {
            r#"<path d="M6.55 1.4h2.9l.32 1.72c.48.14.92.38 1.3.7l1.66-.55 1.45 2.5-1.35 1.1c.08.34.12.7.12 1.07s-.04.73-.12 1.07l1.35 1.1-1.45 2.5-1.66-.55a4.3 4.3 0 0 1-1.3.7L9.45 14.6h-2.9l-.32-1.72a4.3 4.3 0 0 1-1.3-.7l-1.66.55-1.45-2.5 1.35-1.1A4.4 4.4 0 0 1 3.05 7.94c0-.37.04-.73.12-1.07l-1.35-1.1 1.45-2.5 1.66.55c.38-.32.82-.56 1.3-.7L6.55 1.4z"/><circle cx="8" cy="8" r="2.2"/>"#
        }
        "move" => {
            r#"<path d="M8 1.8v12.4M1.8 8h12.4M5.6 4.2 8 1.8l2.4 2.4M5.6 11.8 8 14.2l2.4-2.4M4.2 5.6 1.8 8l2.4 2.4M11.8 5.6 14.2 8l-2.4 2.4"/>"#
        }
        "objects" => {
            r#"<path d="M8 1.8 14 5 8 8.2 2 5z"/><path d="M2 8l6 3.2L14 8" opacity="0.7"/><path d="M2 11l6 3.2 6-3.2" opacity="0.4"/>"#
        }
        "maximize" => {
            r#"<path d="M2.5 6V3a.5.5 0 0 1 .5-.5h3M10 2.5h3a.5.5 0 0 1 .5.5v3M13.5 10v3a.5.5 0 0 1-.5.5h-3M6 13.5H3a.5.5 0 0 1-.5-.5v-3"/>"#
        }
        "restore" => {
            r#"<path d="M6.2 2.5v3.7H2.5M9.8 13.5V9.8h3.7M13.5 6.2H9.8V2.5M2.5 9.8h3.7v3.7"/>"#
        }
        "pane-collapse" => {
            r#"<path d="M2.6 9.4h4v4M13.4 6.6h-4v-4"/><path d="m9.4 6.6 4.2-4.2M2.4 13.6l4.2-4.2"/>"#
        }
        "pane-expand" => {
            r#"<path d="M9.8 2.4h3.8v3.8M6.2 13.6H2.4V9.8"/><path d="m13.6 2.4-4.4 4.4M2.4 13.6l4.4-4.4"/>"#
        }
        "info" => {
            r#"<path d="M8 7.4v4.4"/><circle cx="8" cy="4.6" r="0.9" fill="currentColor" stroke="none"/>"#
        }
        "indicators" => {
            r#"<path d="M1.5 12.5 5 7l2.5 3.5L11 4l3.5 5"/><circle cx="11" cy="4" r="1.4" fill="currentColor" stroke="none"/>"#
        }
        "bell" => {
            r#"<path d="M8 2a4 4 0 0 0-4 4v2.5L2.5 11v1h11v-1L12 8.5V6a4 4 0 0 0-4-4z"/><path d="M6.5 13.5a1.5 1.5 0 0 0 3 0"/>"#
        }
        "clock" => r#"<circle cx="8" cy="8" r="6.2"/><path d="M8 4.8V8l2.4 1.6"/>"#,
        "star" => {
            r#"<path d="M8 2.2l1.75 3.55 3.9.55-2.8 2.75.65 3.9L8 11.1l-3.5 1.85.65-3.9-2.8-2.75 3.9-.55z"/>"#
        }
        "trash" => {
            r#"<path d="M2.5 4.3h11M6.2 4.3V3.1a1 1 0 0 1 1-1h1.6a1 1 0 0 1 1 1v1.2M4.2 4.3l.5 9.1a1.15 1.15 0 0 0 1.15 1.1h4.3a1.15 1.15 0 0 0 1.15-1.1l.5-9.1M6.5 7v4.5M9.5 7v4.5"/>"#
        }
        _ => return None,
    };
    Some(s16(body))
}

/// The raw markup of icon `id`: a registered one first, then the built-in table, then the
/// drawings registry. `None` for an unknown id.
pub fn markup(id: &str) -> Option<String> {
    if let Some(found) = REGISTERED.with(|r| r.borrow().get(id).cloned()) {
        return Some(found);
    }
    if let Some(found) = builtin(id) {
        return Some(found);
    }
    let drawings = crate::icons::icon(id);
    (!drawings.is_empty()).then(|| drawings.to_string())
}

/// `iconAt`: the markup pinned to an explicit pixel size (empty for an unknown id).
pub fn icon_at(id: &str, px: u32) -> String {
    match markup(id) {
        Some(svg) => svg.replacen("<svg ", &format!("<svg width=\"{px}\" height=\"{px}\" "), 1),
        None => String::new(),
    }
}
