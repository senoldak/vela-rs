//! The pure half of the indicator settings dialog: how an input schema becomes tabs, groups
//! and rows (the helpers at the bottom of `IndicatorInputsDialog.ts`), and the small
//! text conversions of the time and session controls. No DOM here, so it is tested natively.

use vela_core::i18n::{MessageLike, msg, with_values};
use vela_core::model::{InputGroupHelp, InputSchema, InputType, InputValue, Inputs, input_visible};

use super::Tr;

/// The `input.source` choices.
pub const SOURCES: [&str; 8] = [
    "close", "open", "high", "low", "hl2", "hlc3", "ohlc4", "volume",
];

/// The settings-dialog tab hosting inputs that declare no `tab=`.
pub const DEFAULT_INPUT_TAB: &str = "Inputs";
/// The settings-dialog tab hosting declaration props.
pub const PROPS_TAB: &str = "Properties";

/// An input as the dialog renders it: `prop` marks a declaration-prop entry, whose reads and
/// writes go to the prop bag and whose commits carry the `prop` kind.
#[derive(Clone, Debug)]
pub struct Decl {
    pub schema: InputSchema,
    pub prop: bool,
}

/// One settings-dialog tab: its strip label and the inputs it hosts, in declaration order.
#[derive(Clone, Debug)]
pub struct InputTab {
    pub name: String,
    pub inputs: Vec<Decl>,
}

/// Partition inputs into tabs. Inputs without a `tab=` land on the default "Inputs" tab
/// (an explicit `tab='Inputs'` merges with it), which leads the strip whenever it has
/// members; declared tabs follow in first-seen order.
pub fn tab_inputs(inputs: &[InputSchema]) -> Vec<InputTab> {
    let mut tabs: Vec<InputTab> = Vec::new();
    for inp in inputs {
        let name = match inp.tab.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => DEFAULT_INPUT_TAB.to_string(),
        };
        let decl = Decl {
            schema: inp.clone(),
            prop: false,
        };
        match tabs.iter_mut().find(|t| t.name == name) {
            Some(tab) => tab.inputs.push(decl),
            None => tabs.push(InputTab {
                name,
                inputs: vec![decl],
            }),
        }
    }
    if let Some(at) = tabs.iter().position(|t| t.name == DEFAULT_INPUT_TAB)
        && at > 0
    {
        let tab = tabs.remove(at);
        tabs.insert(0, tab);
    }
    tabs
}

/// One `group=` section: its heading (when named), the heading's translatable text and help
/// mark, and its rows (inputs sharing an `inline=` id sit on one row).
#[derive(Clone, Debug)]
pub struct InputGroup {
    pub name: Option<String>,
    pub label: Option<MessageLike>,
    pub help: Option<InputGroupHelp>,
    pub rows: Vec<Vec<Decl>>,
}

/// Bucket inputs into `group=` sections (first-seen order, ungrouped inputs stay where
/// declared) and, within each, collapse inputs that share an `inline=` id onto one row.
pub fn group_inputs(inputs: &[Decl]) -> Vec<InputGroup> {
    let mut buckets: Vec<(Option<String>, Vec<&Decl>)> = Vec::new();
    for d in inputs {
        let g = d.schema.group.clone().filter(|g| !g.is_empty());
        match buckets.iter_mut().find(|(name, _)| *name == g) {
            Some((_, members)) => members.push(d),
            None => buckets.push((g, vec![d])),
        }
    }
    buckets
        .into_iter()
        .map(|(name, members)| {
            let label = members
                .iter()
                .find_map(|m| m.schema.display.as_ref().and_then(|d| d.group.clone()));
            let help = members.iter().find_map(|m| m.schema.group_help.clone());
            let mut rows: Vec<Vec<Decl>> = Vec::new();
            let mut inline_row: Vec<(String, usize)> = Vec::new();
            for m in members {
                let inline = m.schema.inline.clone().filter(|i| !i.is_empty());
                match inline
                    .as_ref()
                    .and_then(|i| inline_row.iter().find(|(k, _)| k == i))
                {
                    Some((_, at)) => rows[*at].push(m.clone()),
                    None => {
                        if let Some(i) = inline {
                            inline_row.push((i, rows.len()));
                        }
                        rows.push(vec![m.clone()]);
                    }
                }
            }
            InputGroup {
                name,
                label,
                help,
                rows,
            }
        })
        .collect()
}

/// A row shows while ANY of its `inline=` members' gates pass.
pub fn row_visible(decls: &[Decl], bag: &Inputs) -> bool {
    decls.iter().any(|d| input_visible(&d.schema.when, bag))
}

/// The display name of an input: its (translated) title with the first letter capitalized.
/// A blank title yields no label (the Pine idiom for a companion `inline=` control).
pub fn name_of(inp: &InputSchema, tr: &Tr) -> String {
    let title = match inp.display.as_ref().and_then(|d| d.title.as_ref()) {
        Some(m) => tr.m(m),
        None => inp.title.clone(),
    };
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// An input's tooltip as the chart's language reads it.
pub fn tooltip_of(inp: &InputSchema, tr: &Tr) -> String {
    match inp.display.as_ref().and_then(|d| d.tooltip.as_ref()) {
        Some(m) => tr.m(m),
        None => inp.tooltip.clone().unwrap_or_default(),
    }
}

/// Whether an input renders as a dropdown (single or multi-select).
pub fn is_select_input(inp: &InputSchema) -> bool {
    inp.input_type != InputType::Bool
        && (inp.options.as_ref().is_some_and(|o| !o.is_empty())
            || inp.input_type == InputType::Source
            || inp.input_type == InputType::Timeframe)
}

/// Controls that size themselves instead of sitting in the 100px column.
pub fn is_fit_control(inp: &InputSchema) -> bool {
    is_select_input(inp)
        || matches!(
            inp.input_type,
            InputType::Color | InputType::Bool | InputType::Session | InputType::Time
        )
}

/// The current value of an input in a bag: the stored value, else its default.
pub fn current_of<'a>(bag: &'a Inputs, inp: &'a InputSchema) -> &'a InputValue {
    bag.get(&inp.key).unwrap_or(&inp.defval)
}

/// A value as a number (text parses, a bool is 0 or 1).
pub fn number_of(v: &InputValue) -> f64 {
    match v {
        InputValue::Number(n) => *n,
        InputValue::Text(s) => s.trim().parse().unwrap_or(0.0),
        InputValue::Bool(b) => f64::from(u8::from(*b)),
    }
}

/// A value as a bool (`Boolean(value)`).
pub fn bool_of(v: &InputValue) -> bool {
    match v {
        InputValue::Bool(b) => *b,
        InputValue::Number(n) => *n != 0.0 && !n.is_nan(),
        InputValue::Text(s) => !s.is_empty(),
    }
}

/// The values resolved for gate evaluation: the stored value, else the input's default.
pub fn values_bag(inputs: &[InputSchema], values: &Inputs) -> Inputs {
    inputs
        .iter()
        .map(|i| {
            (
                i.key.clone(),
                values
                    .get(&i.key)
                    .cloned()
                    .unwrap_or_else(|| i.defval.clone()),
            )
        })
        .collect()
}

/// The `input.timeframe` choices: the value committed (Pine resolution, `""` = the chart's)
/// and the label shown.
pub fn timeframe_options() -> Vec<(&'static str, MessageLike)> {
    let minutes = msg(
        "chart.indicatorSettings.timeframe.minutes",
        "{count, plural, one {# minute} other {# minutes}}",
    );
    let hours = msg(
        "chart.indicatorSettings.timeframe.hours",
        "{count, plural, one {# hour} other {# hours}}",
    );
    let n = |m: &MessageLike, count: f64| with_values(m, [("count", count.into())]);
    vec![
        ("", msg("chart.indicatorSettings.timeframe.chart", "Chart")),
        ("1", n(&minutes, 1.0)),
        ("3", n(&minutes, 3.0)),
        ("5", n(&minutes, 5.0)),
        ("15", n(&minutes, 15.0)),
        ("30", n(&minutes, 30.0)),
        ("45", n(&minutes, 45.0)),
        ("60", n(&hours, 1.0)),
        ("120", n(&hours, 2.0)),
        ("180", n(&hours, 3.0)),
        ("240", n(&hours, 4.0)),
        ("D", msg("chart.indicatorSettings.timeframe.day", "1 day")),
        ("W", msg("chart.indicatorSettings.timeframe.week", "1 week")),
        (
            "M",
            msg("chart.indicatorSettings.timeframe.month", "1 month"),
        ),
    ]
}

/// Accept `HH:MM`, `H:MM` or `HHMM` and return a padded `HH:MM`, or `None` when unusable.
pub fn normalize_time_input(raw: &str) -> Option<String> {
    let t = raw.trim();
    let (hh, mm) = match t.split_once(':') {
        Some((h, m)) if (1..=2).contains(&h.len()) && m.len() == 2 => (h, m),
        None if t.len() == 4 && t.is_ascii() => (&t[..2], &t[2..]),
        _ => return None,
    };
    if !hh.bytes().all(|b| b.is_ascii_digit()) || !mm.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (u32, u32) = (hh.parse().ok()?, mm.parse().ok()?);
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

/// Split a Pine `HHMM-HHMM` session into two `HH:MM` strings (defaults `09:00` to `16:00`).
pub fn session_to_times(session: &str) -> (String, String) {
    let s = session.trim();
    let valid = s.len() == 9
        && s.as_bytes()[4] == b'-'
        && s.bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || b.is_ascii_digit());
    if !valid {
        return ("09:00".to_string(), "16:00".to_string());
    }
    (
        format!("{}:{}", &s[0..2], &s[2..4]),
        format!("{}:{}", &s[5..7], &s[7..9]),
    )
}

/// Recombine two `HH:MM` strings into a Pine `HHMM-HHMM` session string.
pub fn times_to_session(start: &str, end: &str) -> String {
    format!("{}-{}", start.replace(':', ""), end.replace(':', ""))
}

/// A local date and time of day as a `YYYY-MM-DD` date and a 30-minute-snapped `HH:MM`.
pub fn snap_parts(year: i32, month1: u32, day: u32, hour: u32, minute: u32) -> (String, String) {
    let snapped = ((hour * 60 + minute) as f64 / 30.0).round() as u32 * 30;
    (
        format!("{year:04}-{month1:02}-{day:02}"),
        format!("{:02}:{:02}", (snapped / 60) % 24, snapped % 60),
    )
}

/// Split a `YYYY-MM-DD` date and an `HH:MM` time into numbers.
pub fn parse_date_time(date: &str, time: &str) -> Option<(i32, u32, u32, u32, u32)> {
    let mut d = date.trim().split('-');
    let (y, m, day) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() {
        return None;
    }
    let (h, mi) = time.trim().split_once(':')?;
    Some((
        y.parse().ok()?,
        m.parse().ok().filter(|m| (1..=12).contains(m))?,
        day.parse().ok().filter(|d| (1..=31).contains(d))?,
        h.parse().ok().filter(|h| *h < 24)?,
        mi.parse().ok().filter(|m| *m < 60)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(key: &str) -> InputSchema {
        InputSchema::new(key, key, InputType::Int, 1)
    }

    #[test]
    fn the_default_tab_leads_and_declared_tabs_follow_in_first_seen_order() {
        let mut a = schema("a");
        a.tab = Some("Style".into());
        let b = schema("b");
        let mut c = schema("c");
        c.tab = Some("Inputs".into());
        let tabs = tab_inputs(&[a, b, c]);
        assert_eq!(
            tabs.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Inputs", "Style"]
        );
        assert_eq!(tabs[0].inputs.len(), 2);
    }

    #[test]
    fn inline_inputs_share_a_row_and_groups_keep_first_seen_order() {
        let mut a = schema("a");
        a.group = Some("G".into());
        a.inline = Some("x".into());
        let mut b = schema("b");
        b.group = Some("G".into());
        b.inline = Some("x".into());
        let c = schema("c");
        let decls: Vec<Decl> = [a, c, b]
            .into_iter()
            .map(|schema| Decl {
                schema,
                prop: false,
            })
            .collect();
        let groups = group_inputs(&decls);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name.as_deref(), Some("G"));
        assert_eq!(groups[0].rows.len(), 1);
        assert_eq!(groups[0].rows[0].len(), 2);
        assert_eq!(groups[1].name, None);
    }

    #[test]
    fn time_text_is_normalized() {
        assert_eq!(normalize_time_input("9:30").as_deref(), Some("09:30"));
        assert_eq!(normalize_time_input("0930").as_deref(), Some("09:30"));
        assert_eq!(normalize_time_input("24:00"), None);
        assert_eq!(normalize_time_input("12:7"), None);
    }

    fn titled(title: &str) -> InputSchema {
        InputSchema::new("intrabarTf", title, InputType::Timeframe, "1")
    }

    // ── ported from `test/inputs-ui-name.test.ts` ──

    #[test]
    fn an_explicitly_empty_title_renders_no_label() {
        // `input.timeframe('1', '', inline='intrabar')` must not surface its key as the label.
        assert_eq!(name_of(&titled(""), &Tr::new(None)), "");
    }

    #[test]
    fn the_first_letter_of_a_provided_title_is_capitalized() {
        let tr = Tr::new(None);
        assert_eq!(name_of(&titled("source"), &tr), "Source");
        assert_eq!(
            name_of(&titled("Intrabar Precision"), &tr),
            "Intrabar Precision"
        );
    }

    #[test]
    fn a_key_derived_title_is_shown_as_is() {
        // `mapInputs` fills a missing title with the key: a non-empty title is shown.
        assert_eq!(name_of(&titled("intrabarTf"), &Tr::new(None)), "IntrabarTf");
    }

    // ── ported from `test/inputs-ui-tabs.test.ts` (tabInputs) ──

    fn on_tab(key: &str, tab: Option<&str>) -> InputSchema {
        let mut s = schema(key);
        s.tab = tab.map(str::to_string);
        s
    }

    fn keys(tab: &InputTab) -> Vec<&str> {
        tab.inputs.iter().map(|d| d.schema.key.as_str()).collect()
    }

    #[test]
    fn every_input_lands_on_the_default_tab_when_none_declares_one() {
        let tabs = tab_inputs(&[on_tab("a", None), on_tab("b", None)]);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, DEFAULT_INPUT_TAB);
        assert_eq!(keys(&tabs[0]), ["a", "b"]);
    }

    #[test]
    fn the_default_tab_leads_declared_tabs_follow_in_first_seen_order() {
        let tabs = tab_inputs(&[
            on_tab("s1", Some("Style")),
            on_tab("a", None),
            on_tab("v1", Some("Visibility")),
            on_tab("s2", Some("Style")),
        ]);
        assert_eq!(
            tabs.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            [DEFAULT_INPUT_TAB, "Style", "Visibility"]
        );
        assert_eq!(keys(&tabs[1]), ["s1", "s2"]);
    }

    #[test]
    fn an_explicit_inputs_tab_merges_with_the_default_tab() {
        let tabs = tab_inputs(&[on_tab("a", Some("Inputs")), on_tab("b", None)]);
        assert_eq!(tabs.len(), 1);
        assert_eq!(keys(&tabs[0]), ["a", "b"]);
    }

    #[test]
    fn an_empty_tab_name_counts_as_unset() {
        let tabs = tab_inputs(&[on_tab("a", Some(""))]);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, DEFAULT_INPUT_TAB);
    }

    #[test]
    fn the_default_tab_is_omitted_when_every_input_declares_another() {
        let tabs = tab_inputs(&[on_tab("s1", Some("Style")), on_tab("s2", Some("Style"))]);
        assert_eq!(
            tabs.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Style"]
        );
    }

    // ── ported from `test/inputs-ui-time.test.ts` (normalizeTimeInput) ──

    #[test]
    fn padded_hh_mm_is_accepted() {
        assert_eq!(normalize_time_input("09:30").as_deref(), Some("09:30"));
        assert_eq!(normalize_time_input("23:59").as_deref(), Some("23:59"));
    }

    #[test]
    fn a_single_digit_hour_is_padded() {
        assert_eq!(normalize_time_input("9:30").as_deref(), Some("09:30"));
    }

    #[test]
    fn hhmm_without_a_colon_is_accepted() {
        assert_eq!(normalize_time_input("0930").as_deref(), Some("09:30"));
        assert_eq!(normalize_time_input("0000").as_deref(), Some("00:00"));
    }

    #[test]
    fn out_of_range_and_empty_values_are_rejected() {
        assert_eq!(normalize_time_input("24:00"), None);
        assert_eq!(normalize_time_input("12:60"), None);
        assert_eq!(normalize_time_input("abc"), None);
        assert_eq!(normalize_time_input(""), None);
        // Four bytes that are not four digits, and not a char boundary at 2, never panic.
        assert_eq!(normalize_time_input("\u{e9}\u{e9}"), None);
    }

    #[test]
    fn sessions_round_trip() {
        let (a, b) = session_to_times("0930-1600");
        assert_eq!((a.as_str(), b.as_str()), ("09:30", "16:00"));
        assert_eq!(times_to_session(&a, &b), "0930-1600");
        assert_eq!(session_to_times("garbage").0, "09:00");
    }

    #[test]
    fn a_time_snaps_to_half_hours() {
        assert_eq!(
            snap_parts(2025, 3, 4, 9, 44),
            ("2025-03-04".into(), "09:30".into())
        );
        assert_eq!(snap_parts(2025, 3, 4, 23, 50).1, "00:00");
        assert_eq!(
            parse_date_time("2025-03-04", "09:30"),
            Some((2025, 3, 4, 9, 30))
        );
        assert_eq!(parse_date_time("2025-13-04", "09:30"), None);
    }
}
