//! Dumps every built-in drawing tool as JSON: its registry entry, and what it
//! answers (geometry, hit-tests, settings, serialization) under a spread of
//! anchor sets, drags and settings patches. The output is the Rust half of a
//! parity check against the original TypeScript drawings (see the README).
//!
//! Usage: `cargo run --release --example dump_drawings > drawings.json`

use serde_json::{Value, json};
use vela_core::drawings::{
    Drawing, DrawingInit, DrawingPoint, DrawingRegistry, DrawingTypeMeta, FieldKind, FreeAxis,
    LinearProjector, PlacementMode, Projector, Props, SettingsSchema,
};
use vela_core::i18n::{MessageLike, source_text};
use vela_core::model::Ohlcv;

const PANE: &str = "price";
const MINUTE: f64 = 60_000.0;
/// Hit-test tolerance, in px.
const TOL: f64 = 5.0;
/// Anchors a variable-count tool is probed with beyond its minimum.
const EXTRA_ANCHORS: usize = 3;

/// Lehmer generator: every product stays under 2^53, so the numbers are exact in JavaScript too.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = (self.0 * 48271) % 2_147_483_647;
        self.0
    }
}

/// One-minute bars from time 0, inside the sample projector's view.
fn bars() -> Vec<Ohlcv> {
    let mut rng = Rng(2024);
    let mut close_cents: i64 = 12_000;
    (0..1000)
        .map(|i| {
            let open_cents = close_cents;
            close_cents = (close_cents + (rng.next() % 121) as i64 - 60).clamp(7_000, 18_000);
            let high_cents = open_cents.max(close_cents) + (rng.next() % 40) as i64;
            let low_cents = open_cents.min(close_cents) - (rng.next() % 40) as i64;
            let volume = 100 + rng.next() % 900;
            Ohlcv {
                time: i * 60_000,
                open: open_cents as f64 / 100.0,
                high: high_cents as f64 / 100.0,
                low: low_cents as f64 / 100.0,
                close: close_cents as f64 / 100.0,
                volume: Some(volume as f64),
            }
        })
        .collect()
}

/// Anchor candidates spread across the view: whole minutes, prices on a cent grid.
fn anchor_pool() -> Vec<DrawingPoint> {
    let mut rng = Rng(77);
    (0..16)
        .map(|_| {
            let minute = 60 + rng.next() % 860;
            let cents = 8_000 + rng.next() % 9_000;
            DrawingPoint {
                time: minute as f64 * MINUTE,
                price: cents as f64 / 100.0,
            }
        })
        .collect()
}

fn text(m: &MessageLike) -> Value {
    Value::String(source_text(m))
}

fn opt_text(m: Option<&MessageLike>) -> Value {
    m.map(text).unwrap_or(Value::Null)
}

fn points(pts: &[(f64, f64)]) -> Value {
    Value::Array(pts.iter().map(|p| json!([p.0, p.1])).collect())
}

fn anchors(pts: &[DrawingPoint]) -> Value {
    serde_json::to_value(pts).unwrap_or(Value::Null)
}

fn schema(s: &SettingsSchema) -> Value {
    let fields: Vec<Value> = s
        .fields
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "label": text(&f.label),
                "kind": f.kind.as_str(),
                "min": f.min,
                "max": f.max,
                "step": f.step,
                "options": f.options.as_ref().map(|opts| opts.iter().map(|o| json!({ "value": o.value, "label": text(&o.label) })).collect::<Vec<_>>()),
                "group": f.group.map(|g| g.as_str()),
                "when": f.when.as_ref().map(|w| json!({ "path": w.path, "in": w.any_of })),
                "fallback": f.fallback,
                "rowLabel": opt_text(f.row_label.as_ref()),
            })
        })
        .collect();
    json!({ "fields": fields, "textIsContent": s.text_is_content })
}

/// The state a drawing persists, without its identity and creation time.
fn state(d: &dyn Drawing) -> Value {
    let doc = d.serialize();
    json!({
        "anchors": anchors(&doc.anchors),
        "style": doc.style,
        "text": doc.text,
        "locked": doc.locked,
        "visible": doc.visible,
        "props": doc.props,
        "showOn": doc.show_on,
    })
}

fn free_axis(free: FreeAxis) -> &'static str {
    match free {
        FreeAxis::Both => "both",
        FreeAxis::X => "x",
        FreeAxis::Y => "y",
        FreeAxis::None => "none",
    }
}

/// Everything a drawing answers about its geometry.
fn probe(d: &dyn Drawing, proj: &dyn Projector) -> Value {
    let handles = d.handle_points(proj);
    let mut hits = String::new();
    let mut grabbed = Vec::new();
    for gy in 0..=12 {
        for gx in 0..=20 {
            let (px, py) = (gx as f64 * 50.0, gy as f64 * 50.0);
            hits.push(if d.hit_test(px, py, proj, TOL) {
                '1'
            } else {
                '0'
            });
            grabbed.push(d.hit_handle(px, py, proj, TOL).map_or(-1, |i| i as i64));
        }
    }
    let at_handles: Vec<i64> = handles
        .iter()
        .map(|h| {
            d.hit_handle(h.0 + 1.0, h.1 - 1.0, proj, TOL)
                .map_or(-1, |i| i as i64)
        })
        .collect();
    let on_handles: Vec<bool> = handles
        .iter()
        .map(|h| d.hit_test(h.0, h.1, proj, TOL))
        .collect();
    let anchor_schema = d.anchor_schema();
    json!({
        "anchorSchema": {
            "min": anchor_schema.min,
            "max": anchor_schema.max,
            "slots": anchor_schema.slots.iter().map(|s| json!({ "role": s.role, "free": free_axis(s.free) })).collect::<Vec<_>>(),
        },
        "isComplete": d.is_complete(),
        "placementMode": match d.placement_mode() {
            PlacementMode::Click => "click",
            PlacementMode::Drag => "drag",
            PlacementMode::Freehand => "freehand",
        },
        "handlePoints": points(&handles),
        "bounds": d.bounds(proj).map(|b| json!({ "x": b.x, "y": b.y, "w": b.w, "h": b.h })),
        "priceRange": d.price_range().map(|r| json!({ "min": r.min, "max": r.max })),
        "timeExtent": d.time_extent().map(|r| json!({ "min": r.min, "max": r.max })),
        "hitGrid": hits,
        "handleGrid": grabbed,
        "hitHandleAtHandles": at_handles,
        "hitTestAtHandles": on_handles,
        "schema": schema(&d.schema()),
        "placeholderText": opt_text(d.placeholder_text().as_ref()),
        "editableLevels": d.editable_levels(),
        "state": state(d),
    })
}

fn create(
    registry: &DrawingRegistry,
    type_key: &str,
    pts: &[DrawingPoint],
    props: Option<Props>,
) -> Option<Box<dyn Drawing>> {
    let init = DrawingInit {
        id: Some("dw-1".to_string()),
        anchors: pts.to_vec(),
        created_at: Some(0.0),
        props,
        ..DrawingInit::on_pane(PANE)
    };
    registry.create(type_key, init)
}

/// The values a settings field is exercised with.
fn candidates(
    kind: FieldKind,
    min: Option<f64>,
    max: Option<f64>,
    options: &[String],
) -> Vec<Value> {
    match kind {
        FieldKind::Boolean => vec![json!(true), json!(false)],
        FieldKind::Number => {
            let mut out = vec![json!(min.unwrap_or(3.0))];
            if let Some(max) = max {
                out.push(json!(max));
            }
            out
        }
        FieldKind::Opacity => vec![json!(0.5)],
        FieldKind::Color => vec![json!("#112233")],
        FieldKind::LineStyle => vec![json!("dashed")],
        FieldKind::Select => options.iter().map(|o| json!(o)).collect(),
        FieldKind::Text => vec![json!("Hello")],
    }
}

fn dump_type(
    registry: &DrawingRegistry,
    meta: &DrawingTypeMeta,
    proj: &dyn Projector,
    pool: &[DrawingPoint],
) -> Value {
    let Some(empty) = create(registry, meta.type_key, &[], None) else {
        return Value::Null;
    };
    let anchor_schema = empty.anchor_schema();
    let top = if anchor_schema.max > anchor_schema.min {
        (anchor_schema.min + EXTRA_ANCHORS)
            .min(anchor_schema.max)
            .min(pool.len())
    } else {
        anchor_schema.min
    };
    let mut counts = vec![
        0,
        anchor_schema.min.saturating_sub(1),
        anchor_schema.min,
        top,
    ];
    counts.sort_unstable();
    counts.dedup();

    let mut runs = Vec::new();
    for &count in &counts {
        let pts = &pool[..count.min(pool.len())];
        let Some(d) = create(registry, meta.type_key, pts, None) else {
            continue;
        };
        let moved = d.translate_body(2.0 * MINUTE, -3.5, pts);
        let handle_count = d.handle_points(proj).len();
        let drags: Vec<Value> = (0..handle_count)
            .filter_map(|h| {
                let mut clone = create(registry, meta.type_key, pts, d.write_props())?;
                clone.drag_handle(h, DrawingPoint { time: 480.0 * MINUTE, price: 111.0 });
                clone.constrain_handle_drag(h);
                Some(json!({ "handle": h, "anchors": anchors(&clone.base().anchors), "handlePoints": points(&clone.handle_points(proj)) }))
            })
            .collect();
        let placed = create(registry, meta.type_key, pts, None).map(|mut clone| {
            clone.on_placed(proj);
            state(clone.as_ref())
        });
        runs.push(json!({
            "anchors": anchors(pts),
            "probe": probe(d.as_ref(), proj),
            "translated": anchors(&moved),
            "drags": drags,
            "placed": placed,
        }));
    }

    // Settings: one patch at a time on a fresh, complete drawing.
    let complete = &pool[..anchor_schema.min.min(pool.len())];
    let mut settings = Vec::new();
    if let Some(d) = create(registry, meta.type_key, complete, None) {
        let mut patches: Vec<(String, Value)> = Vec::new();
        for field in &d.schema().fields {
            let options: Vec<String> = field
                .options
                .iter()
                .flatten()
                .map(|o| o.value.clone())
                .collect();
            for value in candidates(field.kind, field.min, field.max, &options) {
                patches.push((field.path.clone(), value));
            }
        }
        if d.editable_levels().is_some_and(|levels| levels.len() >= 2) {
            patches.push(("levels.0.enabled".to_string(), json!(false)));
            patches.push(("levels.1.color".to_string(), json!("#010203")));
            patches.push(("levels.0.label".to_string(), json!("L")));
        }
        patches.push(("showOn".to_string(), json!(["D", "1"])));
        for (path, value) in patches {
            let Some(mut clone) = create(registry, meta.type_key, complete, None) else {
                continue;
            };
            let mut patch = Props::new();
            patch.insert(path.clone(), value.clone());
            clone.apply_settings(&patch);
            settings.push(
                json!({ "path": path, "value": value, "probe": probe(clone.as_ref(), proj) }),
            );
        }
    }

    // Props read back from garbage: every known key mistyped, plus an unknown one.
    let mut garbage = Props::new();
    garbage.insert("__unknown".to_string(), json!(1));
    for key in empty.write_props().unwrap_or_default().keys() {
        garbage.insert(key.clone(), json!("not the right type"));
    }
    let lenient =
        create(registry, meta.type_key, complete, Some(garbage.clone())).map(|d| state(d.as_ref()));

    json!({
        "type": meta.type_key,
        "meta": {
            "group": meta.group,
            "label": text(&meta.label),
            "icon": meta.icon,
            "defaultStyle": meta.default_style,
            "coversSeries": meta.covers_series,
            "placementHint": opt_text(meta.placement_hint.as_ref()),
        },
        "runs": runs,
        "settings": settings,
        "garbageProps": garbage,
        "lenient": lenient,
    })
}

fn main() {
    let bars = bars();
    let proj = LinearProjector::sample().with_bars(bars.clone());
    let registry = DrawingRegistry::builtin();
    let pool = anchor_pool();
    let types: Vec<Value> = registry
        .entries()
        .iter()
        .map(|meta| dump_type(&registry, meta, &proj, &pool))
        .collect();
    let bars_json: Vec<Value> = bars.iter().map(|b| json!({ "time": b.time, "open": b.open, "high": b.high, "low": b.low, "close": b.close, "volume": b.volume })).collect();
    let out = json!({
        "projector": {
            "paneId": proj.pane_id,
            "timeOrigin": proj.time_origin,
            "msPerPx": proj.ms_per_px,
            "priceTop": proj.price_top,
            "pricePerPx": proj.price_per_px,
            "width": proj.width,
            "height": proj.height,
        },
        "tolerance": TOL,
        "bars": bars_json,
        "types": types,
    });
    println!("{out}");
}
