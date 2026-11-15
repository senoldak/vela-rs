//! Timeframe quick entry: the "type a number to change timeframe" dialog. The grammar is
//! [`parse_timeframe`](super::timeframe::parse_timeframe); this module is the dialog's
//! behavior without a DOM: the hint under the field ([`timeframe_hint`]), what Enter applies
//! ([`timeframe_to_apply`]) and how typed text extends the entry.
//!
//! Left for the browser crate: the dialog and field (the field upper-cases what is typed and
//! takes focus a tick after the dialog opens), and the hint element.

use super::timeframe::parse_timeframe;
use crate::i18n::{MessageLike, Translator, msg, text_of};

pub fn title_message() -> MessageLike {
    msg("widget.timeframeQuick.title", "Change timeframe")
}

/// The codes are what the field accepts, typed as is in every language.
pub fn example_message() -> MessageLike {
    msg("widget.timeframeQuick.example", "e.g. 15, 4H, D, 3M")
}

pub fn invalid_message() -> MessageLike {
    msg("widget.timeframeQuick.invalid", "Not a timeframe")
}

/// The hint line under the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeframeHint {
    pub text: String,
    /// Typed something that is not a timeframe.
    pub invalid: bool,
}

/// The hint for what is typed: the example while empty, the parsed human line when valid,
/// an error otherwise.
pub fn timeframe_hint(raw: &str, i18n: Option<&dyn Translator>) -> TimeframeHint {
    let raw = raw.trim();
    if raw.is_empty() {
        return TimeframeHint {
            text: text_of(i18n, &example_message(), &[]),
            invalid: false,
        };
    }
    let parsed = parse_timeframe(raw, i18n);
    if parsed.valid {
        TimeframeHint {
            text: parsed.label.unwrap_or_default(),
            invalid: false,
        }
    } else {
        TimeframeHint {
            text: text_of(i18n, &invalid_message(), &[]),
            invalid: true,
        }
    }
}

/// Enter: the canonical timeframe to apply, when the field holds a valid one.
pub fn timeframe_to_apply(raw: &str) -> Option<String> {
    let parsed = parse_timeframe(raw, None);
    if parsed.valid { parsed.canonical } else { None }
}

/// The field's text after `type(text)` on an open entry: appended, upper-cased.
pub fn extend_entry(current: &str, typed: &str) -> String {
    format!("{current}{}", typed.to_uppercase())
}

/// The field's text when the entry opens with a seed.
pub fn seed_entry(seed: &str) -> String {
    seed.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_shows_the_example_while_empty() {
        for raw in ["", "   "] {
            assert_eq!(
                timeframe_hint(raw, None),
                TimeframeHint {
                    text: "e.g. 15, 4H, D, 3M".into(),
                    invalid: false
                }
            );
        }
    }

    #[test]
    fn a_valid_entry_reads_back_as_a_human_line() {
        assert_eq!(timeframe_hint("15", None).text, "15 minutes");
        assert_eq!(timeframe_hint("4H", None).text, "4 hours");
        assert_eq!(timeframe_hint("3M", None).text, "3 months");
        assert!(!timeframe_hint("D", None).invalid);
    }

    #[test]
    fn anything_else_is_flagged_not_a_timeframe() {
        let hint = timeframe_hint("1X", None);
        assert_eq!(
            hint,
            TimeframeHint {
                text: "Not a timeframe".into(),
                invalid: true
            }
        );
        assert!(timeframe_hint("0", None).invalid);
    }

    #[test]
    fn enter_applies_the_canonical_form_only_for_a_valid_entry() {
        assert_eq!(timeframe_to_apply("4H").as_deref(), Some("240"));
        assert_eq!(timeframe_to_apply("30S").as_deref(), Some("30S"));
        assert_eq!(timeframe_to_apply("D").as_deref(), Some("1440"));
        assert_eq!(timeframe_to_apply("nope"), None);
        assert_eq!(timeframe_to_apply(""), None);
    }

    #[test]
    fn typed_text_extends_or_seeds_the_entry_in_upper_case() {
        assert_eq!(extend_entry("1", "h"), "1H");
        assert_eq!(seed_entry("4h"), "4H");
        assert_eq!(text_of(None, &title_message(), &[]), "Change timeframe");
    }
}
