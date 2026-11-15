//! Shared ticker icon: the reference badge. The icon URL comes from the OWNING PROVIDER
//! (`resolve_symbol_icon` on the data provider port: the shells never guess an asset's
//! icon source); no URL, or an image that errors, falls back to a colored-initials badge.
//! The failure cache is keyed by URL and shared, so a missing icon degrades to initials
//! everywhere at once (and everything keeps working fully offline).
//!
//! This module is the decision logic of the original's `tickerIconEl`. Building the
//! element (a `span` with an `img` child, `crossOrigin = "anonymous"` so a canvas it is
//! drawn onto is not tainted, an `error` listener that calls
//! [`IconFailures::mark_failed`]) is the browser crate's.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::palette::categorical_color;

pub use crate::data::base_of;

/// The URLs whose image failed to load. Clones share the set.
#[derive(Clone, Default)]
pub struct IconFailures {
    failed: Rc<RefCell<HashSet<String>>>,
}

impl IconFailures {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_failed(&self, url: &str) {
        self.failed.borrow_mut().insert(url.to_string());
    }

    pub fn has_failed(&self, url: &str) -> bool {
        self.failed.borrow().contains(url)
    }
}

/// What a ticker icon shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TickerIcon {
    /// Load this image; on an error mark it failed and show [`TickerIcon::Initials`].
    Image { url: String },
    /// A colored badge with up to two initials.
    Initials {
        text: String,
        background: &'static str,
    },
}

/// Up to two alphanumeric characters of `name`, upper-cased; `"?"` when there are none.
pub fn initials_of(name: &str) -> String {
    let source = if name.is_empty() { "?" } else { name };
    let text: String = source
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if text.is_empty() {
        "?".to_string()
    } else {
        text
    }
}

/// The initials badge: initials of `base` (else `name`) over a color keyed by `name`.
pub fn initials_badge(base: &str, name: &str) -> TickerIcon {
    TickerIcon::Initials {
        text: initials_of(if base.is_empty() { name } else { base }),
        background: categorical_color(name),
    }
}

/// The ticker icon for a symbol: the provider-resolved icon when `icon_url` is given and
/// has not failed before, a colored initials badge otherwise.
pub fn ticker_icon(
    failures: &IconFailures,
    base: &str,
    name: &str,
    icon_url: Option<&str>,
) -> TickerIcon {
    match icon_url {
        Some(url) if !url.is_empty() && !failures.has_failed(url) => TickerIcon::Image {
            url: url.to_string(),
        },
        _ => initials_badge(base, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_keep_two_alphanumerics_upper_cased() {
        assert_eq!(initials_of("BTCUSDT"), "BT");
        assert_eq!(initials_of("eth-usd"), "ET");
        assert_eq!(initials_of("-x-y-"), "XY");
        assert_eq!(initials_of("7"), "7");
    }

    #[test]
    fn initials_fall_back_to_a_question_mark() {
        assert_eq!(initials_of(""), "?");
        assert_eq!(initials_of("---"), "?");
        assert_eq!(initials_of("\u{4e2d}\u{6587}"), "?");
    }

    #[test]
    fn no_url_or_a_failed_url_degrades_to_the_badge() {
        let failures = IconFailures::new();
        let badge = |icon: TickerIcon| match icon {
            TickerIcon::Initials { text, background } => (text, background),
            TickerIcon::Image { .. } => panic!("expected a badge"),
        };
        let (text, background) = badge(ticker_icon(&failures, "BTC", "BTCUSDT", None));
        assert_eq!(text, "BT");
        assert_eq!(background, categorical_color("BTCUSDT"));
        badge(ticker_icon(&failures, "BTC", "BTCUSDT", Some("")));

        let url = "https://cdn.example/btc.png";
        assert_eq!(
            ticker_icon(&failures, "BTC", "BTCUSDT", Some(url)),
            TickerIcon::Image { url: url.into() }
        );
        failures.mark_failed(url);
        badge(ticker_icon(&failures, "BTC", "BTCUSDT", Some(url)));
        // The failure cache is shared by every clone, so one failure degrades them all.
        assert!(failures.clone().has_failed(url));
    }

    #[test]
    fn the_badge_reads_the_base_before_the_name() {
        match initials_badge("", "ETHUSD") {
            TickerIcon::Initials { text, .. } => assert_eq!(text, "ET"),
            TickerIcon::Image { .. } => panic!("expected a badge"),
        }
        match initials_badge("sol", "ETHUSD") {
            TickerIcon::Initials { text, .. } => assert_eq!(text, "SO"),
            TickerIcon::Image { .. } => panic!("expected a badge"),
        }
    }
}
