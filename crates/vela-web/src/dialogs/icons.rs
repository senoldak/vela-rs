//! The glyphs of the dialogs, from the original's icon registry
//! (`packages/vela-ui/src/lib/icons.ts`): 16x16 grid, 1.2 stroke, `currentColor`.

fn svg16(body: &str, px: u32) -> String {
    format!(
        "<svg viewBox=\"0 0 16 16\" width=\"{px}\" height=\"{px}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" focusable=\"false\">{body}</svg>"
    )
}

/// The markup of icon `id` at `px` pixels, or an empty string for an unknown id.
pub fn icon(id: &str, px: u32) -> String {
    let body = match id {
        "gear" => {
            "<path d=\"M6.55 1.4h2.9l.32 1.72c.48.14.92.38 1.3.7l1.66-.55 1.45 2.5-1.35 1.1c.08.34.12.7.12 1.07s-.04.73-.12 1.07l1.35 1.1-1.45 2.5-1.66-.55a4.3 4.3 0 0 1-1.3.7L9.45 14.6h-2.9l-.32-1.72a4.3 4.3 0 0 1-1.3-.7l-1.66.55-1.45-2.5 1.35-1.1A4.4 4.4 0 0 1 3.05 7.94c0-.37.04-.73.12-1.07l-1.35-1.1 1.45-2.5 1.66.55c.38-.32.82-.56 1.3-.7L6.55 1.4z\"/><circle cx=\"8\" cy=\"8\" r=\"2.2\"/>"
        }
        "burger" => "<path d=\"M2.5 4.5h11M2.5 8h11M2.5 11.5h11\"/>",
        "close" => "<path d=\"m3.8 3.8 8.4 8.4M12.2 3.8l-8.4 8.4\"/>",
        "chevron-down" => "<path d=\"M3.5 6 8 10.5 12.5 6\"/>",
        "info" => {
            "<circle cx=\"8\" cy=\"8\" r=\"6.2\"/><path d=\"M8 7.4v4.4\"/><circle cx=\"8\" cy=\"4.6\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/>"
        }
        "help" => {
            "<circle cx=\"8\" cy=\"8\" r=\"6.2\"/><path d=\"M6.1 6a1.9 1.9 0 1 1 2.85 1.65c-.6.3-.95.7-.95 1.25v.45\"/><circle cx=\"8\" cy=\"11.5\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/>"
        }
        _ => return String::new(),
    };
    svg16(body, px)
}
