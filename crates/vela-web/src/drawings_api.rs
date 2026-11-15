//! The JavaScript-facing drawings operations, shared by [`crate::chart::VelaChart`] and
//! [`crate::app::VelaApp`]. Each takes the chart's [`DrawingsControl`] (the facade's
//! `chart.drawings`, or one the direct chart builds around its own core controller), so both
//! entries behave identically.
//!
//! Drawings are a capability of a chart whose renderer takes user drawings; `arm` and
//! `show_toolbar` say so with an error instead of doing nothing when it does not.

use serde_json::Value;
use vela_core::controls::drawings_control::DrawingsControl;
use vela_core::drawings::DrawingRegistry;
use vela_core::ports::chart_renderer::DrawingMode;
use wasm_bindgen::prelude::*;

fn js_err(message: impl Into<String>) -> JsValue {
    JsError::new(&message.into()).into()
}

fn supported(ctl: &DrawingsControl) -> Result<(), JsValue> {
    if ctl.supported() {
        Ok(())
    } else {
        Err(js_err(
            "vela: this chart's renderer does not take user drawings",
        ))
    }
}

/// The type keys `arm` accepts, in registration order.
pub fn tool_types() -> Vec<String> {
    DrawingRegistry::builtin()
        .entries()
        .iter()
        .map(|meta| meta.type_key.to_string())
        .collect()
}

/// Arm the tool `type_key` (the next clicks place it), or return to the pointer with `None`.
pub fn arm(ctl: &DrawingsControl, type_key: Option<String>) -> Result<(), JsValue> {
    supported(ctl)?;
    match type_key.as_deref() {
        Some(key) if DrawingRegistry::builtin().get(key).is_none() => {
            Err(js_err(format!("vela: unknown drawing tool \"{key}\"")))
        }
        key => {
            ctl.set_tool(key);
            Ok(())
        }
    }
}

/// Back to the pointer: disarm the tool and leave the measure ruler and the eraser.
pub fn cancel(ctl: &DrawingsControl) -> Result<(), JsValue> {
    supported(ctl)?;
    ctl.set_tool(None);
    ctl.set_mode(None::<DrawingMode>);
    Ok(())
}

pub fn tool(ctl: &DrawingsControl) -> Option<String> {
    ctl.get_tool()
}

/// Undo the last drawing edit; whether there was one.
pub fn undo(ctl: &DrawingsControl) -> bool {
    let can = ctl.can_undo();
    ctl.undo();
    can
}

pub fn redo(ctl: &DrawingsControl) -> bool {
    let can = ctl.can_redo();
    ctl.redo();
    can
}

/// Remove every drawing as one undo step; how many there were.
pub fn clear(ctl: &DrawingsControl) -> usize {
    let ids: Vec<String> = ctl.all().into_iter().map(|d| d.id).collect();
    if !ids.is_empty() {
        ctl.remove_many(&ids);
    }
    ids.len()
}

pub fn count(ctl: &DrawingsControl) -> usize {
    ctl.all().len()
}

/// The drawings document as JSON text (`{ "version": 1, "drawings": [...] }`).
pub fn export(ctl: &DrawingsControl) -> Result<String, JsValue> {
    serde_json::to_string(&ctl.to_json()).map_err(|e| js_err(format!("vela: {e}")))
}

/// Replace the drawings with a document from [`export`]. Unreadable entries (and a
/// document of an unknown version) are dropped, as the core does; a text that is not JSON
/// is an error. Returns how many drawings the chart has afterwards.
pub fn import(ctl: &DrawingsControl, json: &str) -> Result<usize, JsValue> {
    let doc: Value = serde_json::from_str(json)
        .map_err(|e| js_err(format!("vela: the drawings document is not JSON ({e})")))?;
    ctl.from_json(&doc);
    Ok(ctl.all().len())
}

pub fn show_toolbar(ctl: &DrawingsControl, visible: bool) -> Result<(), JsValue> {
    supported(ctl)?;
    ctl.show_toolbar(visible);
    Ok(())
}
