//! The versioned persistence envelope for a chart's user drawings.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::drawing::SerializedDrawing;

pub const DRAWINGS_DOC_VERSION: u32 = 1;

/// A chart's drawings as a document. Its JSON form matches the original's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawingsDocument {
    pub version: u32,
    pub drawings: Vec<SerializedDrawing>,
}

impl Default for DrawingsDocument {
    fn default() -> Self {
        Self {
            version: DRAWINGS_DOC_VERSION,
            drawings: Vec::new(),
        }
    }
}

/// Whether a raw value is a structurally valid serialized drawing (defensive).
pub fn is_valid_serialized(v: &Value) -> bool {
    let Some(d) = v.as_object() else { return false };
    let is_str = |key: &str| d.get(key).is_some_and(Value::is_string);
    let anchors_ok = d
        .get("anchors")
        .and_then(Value::as_array)
        .is_some_and(|anchors| {
            anchors.iter().all(|p| {
                p.get("time").is_some_and(Value::is_number)
                    && p.get("price").is_some_and(Value::is_number)
            })
        });
    is_str("id")
        && is_str("type")
        && is_str("paneId")
        && anchors_ok
        && d.get("style").is_some_and(Value::is_object)
}

/// Coerce untrusted JSON into a valid [`DrawingsDocument`]. Unknown versions
/// or malformed entries are dropped rather than failing the load.
///
/// Stricter than the original in one respect: an entry that passes the
/// structural check but carries a mistyped optional field (a non-object
/// `text`, a string `locked`) is dropped too.
pub fn migrate(doc: &Value) -> DrawingsDocument {
    let version_ok =
        doc.get("version").and_then(Value::as_u64) == Some(u64::from(DRAWINGS_DOC_VERSION));
    let Some(drawings) = doc
        .get("drawings")
        .and_then(Value::as_array)
        .filter(|_| version_ok)
    else {
        return DrawingsDocument::default();
    };
    DrawingsDocument {
        version: DRAWINGS_DOC_VERSION,
        drawings: drawings
            .iter()
            .filter(|v| is_valid_serialized(v))
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(id: &str) -> Value {
        json!({ "id": id, "type": "trendline", "paneId": "price", "anchors": [{ "time": 1, "price": 2 }], "style": { "lineColor": "#fff", "lineWidth": 1, "lineStyle": "solid" } })
    }

    #[test]
    fn malformed_entries_are_dropped() {
        let doc = json!({ "version": 1, "drawings": [record("a"), { "id": 5 }, null, record("b"), { "id": "c", "type": "x", "paneId": "p", "anchors": [{ "time": "t", "price": 1 }], "style": {} }] });
        let migrated = migrate(&doc);
        assert_eq!(
            migrated
                .drawings
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn unknown_versions_and_shapes_yield_an_empty_document() {
        assert_eq!(
            migrate(&json!({ "version": 2, "drawings": [record("a")] })),
            DrawingsDocument::default()
        );
        assert_eq!(migrate(&json!(null)), DrawingsDocument::default());
        assert_eq!(
            migrate(&json!({ "version": 1, "drawings": "nope" })),
            DrawingsDocument::default()
        );
    }

    #[test]
    fn a_document_round_trips_through_json() {
        let doc = migrate(&json!({ "version": 1, "drawings": [record("a")] }));
        let text = serde_json::to_string(&doc).unwrap();
        assert_eq!(
            serde_json::from_str::<DrawingsDocument>(&text).unwrap(),
            doc
        );
    }
}
