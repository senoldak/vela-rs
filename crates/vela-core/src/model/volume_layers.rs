//! Config payloads the built-in volume native indicators push to the renderer's
//! bespoke layers via `setNativeData`. Both indicators draw with their OWN local
//! scale (never through the indicator model), so their units cannot disturb the
//! price autoscale; the payloads carry only cosmetics and shape, and the layers
//! read the chart's bars directly each frame.

use serde::{Deserialize, Serialize};

/// `setNativeData('volume', ...)`: bottom-anchored per-bar volume columns on the price pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeLayerData {
    pub up_color: String,
    pub down_color: String,
    /// Pane-height fraction (0..1) the tallest VISIBLE bar occupies (the layer's own scale).
    pub height_frac: f64,
}

/// `setNativeData('vpvr', ...)`: right-anchored visible-range volume-by-price profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpvrLayerData {
    /// Number of price rows the visible range is bucketed into.
    pub rows: u32,
    /// Pane-width fraction (0..1) of the largest row (the profile's horizontal scale).
    pub width_frac: f64,
    pub up_color: String,
    pub down_color: String,
    /// Outline the point-of-control (highest-volume) row.
    pub show_poc: bool,
    /// Volume fraction (0..1) the highlighted value area covers (rows outside it draw dimmer).
    pub value_area_frac: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn volume_payload_uses_the_original_field_names() {
        let v = VolumeLayerData {
            up_color: "#0f0".into(),
            down_color: "#f00".into(),
            height_frac: 0.25,
        };
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({ "upColor": "#0f0", "downColor": "#f00", "heightFrac": 0.25 })
        );
    }

    #[test]
    fn vpvr_payload_round_trips() {
        let raw = json!({
            "rows": 24, "widthFrac": 0.3, "upColor": "a", "downColor": "b",
            "showPoc": true, "valueAreaFrac": 0.7
        });
        let v: VpvrLayerData = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(v.rows, 24);
        assert!(v.show_poc);
        assert_eq!(serde_json::to_value(&v).unwrap(), raw);
    }

    #[test]
    fn malformed_payload_is_an_error_not_a_panic() {
        assert!(serde_json::from_value::<VolumeLayerData>(json!({ "upColor": 1 })).is_err());
    }
}
