/// Content-addressed id for a plotted element, stable across re-runs of
/// identical source.
///
/// Deliberately NOT an engine's own callsite counter: those renumber whenever
/// the source is edited, so keying persistent state on one would silently
/// rebind to the wrong element. The ordinal disambiguates multiple plots that
/// share a title within one indicator. Every engine MUST mint its series ids
/// through this: it is the identity contract value patches are keyed by.
///
/// `kind` is a series kind (`SeriesKind::as_str`) or one of `fill`,
/// `background`, `hline`, `line`, `box`, `label`, `polyline`, `linefill`, `table`.
pub fn stable_series_id(instance_id: &str, kind: &str, title: &str, ordinal: usize) -> String {
    // Lowercase, then collapse each whitespace run into one dash.
    let lower = title.trim().to_lowercase();
    let mut norm = String::with_capacity(lower.len());
    let mut in_space = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !in_space {
                norm.push('-');
            }
            in_space = true;
        } else {
            norm.push(ch);
            in_space = false;
        }
    }
    format!("{instance_id}:{kind}:{norm}#{ordinal}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_the_title() {
        assert_eq!(
            stable_series_id("macd", "line", "  Signal   Line ", 1),
            "macd:line:signal-line#1"
        );
        assert_eq!(
            stable_series_id("vortex-indicator", "line", "VI+", 0),
            "vortex-indicator:line:vi+#0"
        );
    }
}
