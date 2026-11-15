//! A source's PERMANENT refusal of a series, as opposed to a transient failure
//! (which keeps reading as an empty answer).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Why a source refuses a series for good. `Timeframe`: the series is not
/// served at the requested timeframe (a monthly economic series asked for
/// hourly bars). Open to further reasons; a consumer that meets one it does not
/// know treats it as a plain refusal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DataUnavailableReason {
    Timeframe,
    Other(String),
}

impl DataUnavailableReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Timeframe => "timeframe",
            Self::Other(reason) => reason,
        }
    }
}

impl From<&str> for DataUnavailableReason {
    fn from(reason: &str) -> Self {
        match reason {
            "timeframe" => Self::Timeframe,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl From<String> for DataUnavailableReason {
    fn from(reason: String) -> Self {
        match reason.as_str() {
            "timeframe" => Self::Timeframe,
            _ => Self::Other(reason),
        }
    }
}

impl fmt::Display for DataUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DataUnavailableReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DataUnavailableReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// What a [`DataUnavailableError`] carries: every field plain data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataUnavailableDetails {
    pub reason: DataUnavailableReason,
    /// The refused symbol, as the source names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The refused timeframe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<String>,
    /// The finest timeframe the source serves this symbol at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finest: Option<String>,
    /// The timeframe the source suggests showing this symbol at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl DataUnavailableDetails {
    pub fn new(reason: DataUnavailableReason) -> Self {
        Self {
            reason,
            symbol: None,
            timeframe: None,
            finest: None,
            default: None,
        }
    }
}

/// The error's `name`, which the JSON guard [`is_data_unavailable_error`] reads.
pub const DATA_UNAVAILABLE_ERROR_NAME: &str = "DataUnavailableError";

/// Raised by a data provider (or a custom feed) to say a series is PERMANENTLY
/// not served. On the chart's own series it ends the load, stops live updates
/// and fires `data:unavailable`; on a secondary series the fetch fails with it.
///
/// The default message is built from the fields ([`data_unavailable_message`]),
/// because engines flatten errors to their message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataUnavailableError {
    pub reason: DataUnavailableReason,
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub finest: Option<String>,
    pub default: Option<String>,
    message: String,
}

impl DataUnavailableError {
    /// An error with the message derived from `details`, or `message` when given.
    pub fn new(details: DataUnavailableDetails, message: Option<String>) -> Self {
        let message = message.unwrap_or_else(|| data_unavailable_message(&details));
        let DataUnavailableDetails {
            reason,
            symbol,
            timeframe,
            finest,
            default,
        } = details;
        Self {
            reason,
            symbol,
            timeframe,
            finest,
            default,
            message,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Read an error-shaped JSON value (name, reason, message and the optional
    /// string fields), the way [`is_data_unavailable_error`] judges it.
    pub fn from_value(value: &Value) -> Option<Self> {
        let o = value.as_object()?;
        let optional_string = |key: &str| -> Option<Option<String>> {
            match o.get(key) {
                None => Some(None),
                Some(Value::String(s)) => Some(Some(s.clone())),
                Some(_) => None,
            }
        };
        if o.get("name")?.as_str()? != DATA_UNAVAILABLE_ERROR_NAME {
            return None;
        }
        let reason = o.get("reason")?.as_str()?;
        let message = o.get("message")?.as_str()?;
        Some(Self {
            reason: reason.into(),
            symbol: optional_string("symbol")?,
            timeframe: optional_string("timeframe")?,
            finest: optional_string("finest")?,
            default: optional_string("default")?,
            message: message.to_owned(),
        })
    }
}

impl fmt::Display for DataUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DataUnavailableError {}

/// The self-explaining default message:
/// `GDP is not served at 60; its finest timeframe is 1D`.
pub fn data_unavailable_message(d: &DataUnavailableDetails) -> String {
    let what = d.symbol.as_deref().unwrap_or("The series");
    if d.reason == DataUnavailableReason::Timeframe {
        let mut hints: Vec<String> = Vec::new();
        if let Some(finest) = &d.finest {
            hints.push(format!("its finest timeframe is {finest}"));
        }
        if let Some(default) = &d.default
            && d.finest.as_ref() != Some(default)
        {
            hints.push(format!("its default timeframe is {default}"));
        }
        let timeframe = d.timeframe.as_deref().unwrap_or("this timeframe");
        let tail = if hints.is_empty() {
            String::new()
        } else {
            format!("; {}", hints.join(", "))
        };
        return format!("{what} is not served at {timeframe}{tail}");
    }
    let at = d
        .timeframe
        .as_ref()
        .map(|tf| format!(" at {tf}"))
        .unwrap_or_default();
    format!("{what}{at} is not available ({})", d.reason)
}

/// Whether `value` is an error-shaped [`DataUnavailableError`], from this copy
/// of Vela or any other: it reads the `name` and the fields, never the type.
/// (For a Rust error value, downcast to [`DataUnavailableError`] instead.)
pub fn is_data_unavailable_error(value: &Value) -> bool {
    DataUnavailableError::from_value(value).is_some()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn details(reason: &str) -> DataUnavailableDetails {
        DataUnavailableDetails::new(reason.into())
    }

    #[test]
    fn builds_a_self_explaining_message_from_its_fields() {
        let e = DataUnavailableError::new(
            DataUnavailableDetails {
                symbol: Some("GDP".into()),
                timeframe: Some("60".into()),
                finest: Some("1D".into()),
                ..details("timeframe")
            },
            None,
        );
        assert_eq!(
            e.message(),
            "GDP is not served at 60; its finest timeframe is 1D"
        );
        assert_eq!(e.to_string(), e.message());
        assert_eq!(
            DataUnavailableError::new(
                DataUnavailableDetails {
                    symbol: Some("GDP".into()),
                    timeframe: Some("60".into()),
                    finest: Some("1D".into()),
                    default: Some("M".into()),
                    ..details("timeframe")
                },
                None,
            )
            .message(),
            "GDP is not served at 60; its finest timeframe is 1D, its default timeframe is M"
        );
        assert_eq!(
            DataUnavailableError::new(
                DataUnavailableDetails {
                    symbol: Some("OLD".into()),
                    ..details("delisted")
                },
                None,
            )
            .message(),
            "OLD is not available (delisted)"
        );
        assert_eq!(
            DataUnavailableError::new(details("timeframe"), Some("custom".into())).message(),
            "custom"
        );
    }

    #[test]
    fn message_edge_cases() {
        assert_eq!(
            data_unavailable_message(&details("timeframe")),
            "The series is not served at this timeframe"
        );
        // A default equal to the finest is not repeated.
        let same = DataUnavailableDetails {
            finest: Some("1D".into()),
            default: Some("1D".into()),
            ..details("timeframe")
        };
        assert_eq!(
            data_unavailable_message(&same),
            "The series is not served at this timeframe; its finest timeframe is 1D"
        );
        // A default alone is always reported.
        let only_default = DataUnavailableDetails {
            default: Some("1D".into()),
            ..details("timeframe")
        };
        assert_eq!(
            data_unavailable_message(&only_default),
            "The series is not served at this timeframe; its default timeframe is 1D"
        );
        let with_timeframe = DataUnavailableDetails {
            timeframe: Some("5".into()),
            ..details("delisted")
        };
        assert_eq!(
            data_unavailable_message(&with_timeframe),
            "The series at 5 is not available (delisted)"
        );
    }

    #[test]
    fn it_is_a_std_error_and_downcasts() {
        let e = DataUnavailableError::new(details("timeframe"), None);
        let boxed: Box<dyn std::error::Error> = Box::new(e.clone());
        assert_eq!(boxed.downcast_ref::<DataUnavailableError>(), Some(&e));
    }

    #[test]
    fn reason_round_trips_as_a_plain_string() {
        let d: DataUnavailableDetails =
            serde_json::from_value(json!({ "reason": "timeframe", "finest": "1D" })).unwrap();
        assert_eq!(d.reason, DataUnavailableReason::Timeframe);
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({ "reason": "timeframe", "finest": "1D" })
        );
        let other: DataUnavailableDetails =
            serde_json::from_value(json!({ "reason": "delisted" })).unwrap();
        assert_eq!(
            other.reason,
            DataUnavailableReason::Other("delisted".into())
        );
        assert_eq!(other.reason.as_str(), "delisted");
        assert_eq!(
            DataUnavailableReason::from("timeframe".to_owned()),
            DataUnavailableReason::Timeframe
        );
    }

    #[test]
    fn accepts_an_error_shaped_value_by_name_and_fields() {
        assert!(is_data_unavailable_error(
            &json!({ "name": "DataUnavailableError", "reason": "timeframe", "message": "x" })
        ));
        assert!(is_data_unavailable_error(&json!({
            "name": "DataUnavailableError", "reason": "timeframe", "message": "GDP is not served at 60", "finest": "1D"
        })));
        let e = DataUnavailableError::from_value(&json!({
            "name": "DataUnavailableError", "reason": "timeframe", "message": "m", "symbol": "S", "default": "D"
        }))
        .unwrap();
        assert_eq!(e.symbol.as_deref(), Some("S"));
        assert_eq!(e.default.as_deref(), Some("D"));
        assert_eq!(e.message(), "m");
    }

    #[test]
    fn rejects_anything_else() {
        assert!(!is_data_unavailable_error(
            &json!({ "name": "Error", "message": "boom" })
        ));
        assert!(!is_data_unavailable_error(
            &json!({ "name": "DataUnavailableError", "message": "no reason" })
        ));
        assert!(!is_data_unavailable_error(
            &json!({ "name": "DataUnavailableError", "reason": 3, "message": "x" })
        ));
        assert!(!is_data_unavailable_error(&json!({
            "name": "DataUnavailableError", "reason": "timeframe", "message": "x", "finest": 1440
        })));
        assert!(!is_data_unavailable_error(&json!({
            "name": "DataUnavailableError", "reason": "timeframe", "message": "x", "finest": null
        })));
        assert!(!is_data_unavailable_error(
            &json!({ "name": "OtherError", "reason": "timeframe", "message": "x" })
        ));
        assert!(!is_data_unavailable_error(
            &json!({ "reason": "timeframe", "message": "x" })
        ));
        assert!(!is_data_unavailable_error(
            &json!({ "name": "DataUnavailableError", "reason": "timeframe" })
        ));
        assert!(!is_data_unavailable_error(&Value::Null));
        assert!(!is_data_unavailable_error(&json!("DataUnavailableError")));
    }
}
