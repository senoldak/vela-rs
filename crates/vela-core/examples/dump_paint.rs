//! Dumps what the drawing-tool painter draws, as recorded canvas call streams:
//! every built-in tool under a spread of anchor sets, interaction states, text
//! and settings patches. Each scenario carries the recipe that produced it, so
//! the parity script can replay it on the original TypeScript painter (see the
//! README).
//!
//! Usage: `cargo run --release --example dump_paint > paint.json`

use std::collections::HashSet;

use serde_json::{Value, json};
use vela_core::drawings::{
    Drawing, DrawingInit, DrawingPoint, DrawingRegistry, DrawingText, FieldKind, LinearProjector,
    Projector, Props,
};
use vela_core::model::Ohlcv;
use vela_core::render::canvas::RecordingCanvas;
use vela_core::render::drawings::painter::{DrawingPainter, DrawingTheme, PaintTargets};

const PANE: &str = "price";
const MINUTE: f64 = 60_000.0;
const ID: &str = "dw-1";
/// Anchors a variable-count tool is painted with beyond its minimum.
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

/// How one scenario is built and painted; replayed as is by the parity script.
#[derive(Clone, Default)]
struct Recipe {
    anchors: Vec<DrawingPoint>,
    text: Option<Value>,
    patch: Option<(String, Value)>,
    placed: bool,
    ghost: bool,
    selected: bool,
    hovered: bool,
}

fn build(
    registry: &DrawingRegistry,
    type_key: &str,
    recipe: &Recipe,
    proj: &dyn Projector,
) -> Option<Box<dyn Drawing>> {
    let text = recipe
        .text
        .as_ref()
        .and_then(|t| serde_json::from_value::<DrawingText>(t.clone()).ok());
    let init = DrawingInit {
        id: Some(ID.to_string()),
        anchors: recipe.anchors.clone(),
        created_at: Some(0.0),
        text,
        ..DrawingInit::on_pane(PANE)
    };
    let mut d = registry.create(type_key, init)?;
    if let Some((path, value)) = &recipe.patch {
        let mut patch = Props::new();
        patch.insert(path.clone(), value.clone());
        d.apply_settings(&patch);
    }
    if recipe.placed {
        d.on_placed(proj);
    }
    Some(d)
}

fn run(
    registry: &DrawingRegistry,
    type_key: &str,
    recipe: &Recipe,
    proj: &dyn Projector,
    theme: &DrawingTheme,
) -> Option<Value> {
    let d = build(registry, type_key, recipe, proj)?;
    let canvas = RecordingCanvas::new();
    let mut painter = DrawingPainter::new();
    if recipe.ghost {
        painter.paint_ghost(&canvas, d.as_ref(), proj, theme);
    } else {
        let mut targets = PaintTargets::default();
        if recipe.selected {
            targets.selected = HashSet::from([ID.to_string()]);
        }
        if recipe.hovered {
            targets.hovered = Some(ID.to_string());
        }
        painter.paint_all(&canvas, &[d.as_ref()], proj, theme, &targets);
    }
    Some(json!({
        "anchors": recipe.anchors,
        "text": recipe.text,
        "patch": recipe.patch.as_ref().map(|(path, value)| json!({ "path": path, "value": value })),
        "placed": recipe.placed,
        "ghost": recipe.ghost,
        "selected": recipe.selected,
        "hovered": recipe.hovered,
        "ops": canvas.to_json(),
    }))
}

fn main() {
    let bars = bars();
    let proj = LinearProjector::sample().with_bars(bars.clone());
    let registry = DrawingRegistry::builtin();
    let pool = anchor_pool();
    let theme_json = json!({ "background": "#131722", "textColor": "#d1d4dc", "fontFamily": "Inter, sans-serif" });
    let theme = DrawingTheme::from_value(&theme_json);
    // Through the model and back, so the recipe carries every field with its default.
    let full = |text: Value| {
        serde_json::from_value::<DrawingText>(text)
            .ok()
            .and_then(|t| serde_json::to_value(t).ok())
            .unwrap_or(Value::Null)
    };
    let label = full(json!({ "value": "Hello world" }));
    let long_label =
        full(json!({ "value": "A longer note that has to wrap\nover lines", "color": "#ffcc00" }));

    let mut types = Vec::new();
    for meta in registry.entries() {
        let key = meta.type_key;
        let Some(empty) = registry.create(key, DrawingInit::on_pane(PANE)) else {
            continue;
        };
        let schema = empty.anchor_schema();
        let top = if schema.max > schema.min {
            (schema.min + EXTRA_ANCHORS).min(schema.max).min(pool.len())
        } else {
            schema.min
        };
        let mut counts = vec![0, schema.min.saturating_sub(1), schema.min, top];
        counts.sort_unstable();
        counts.dedup();

        let mut recipes = Vec::new();
        for &count in &counts {
            let anchors = pool[..count.min(pool.len())].to_vec();
            let base = Recipe {
                anchors,
                ..Recipe::default()
            };
            recipes.push(base.clone());
            recipes.push(Recipe {
                selected: true,
                hovered: true,
                ..base.clone()
            });
            recipes.push(Recipe {
                hovered: true,
                ..base.clone()
            });
            recipes.push(Recipe {
                ghost: true,
                ..base.clone()
            });
            recipes.push(Recipe {
                placed: true,
                ..base.clone()
            });
            recipes.push(Recipe {
                text: Some(label.clone()),
                ..base.clone()
            });
            recipes.push(Recipe {
                text: Some(long_label.clone()),
                selected: true,
                ..base
            });
        }

        // Settings: one patch at a time on a complete drawing, with and without text.
        let complete = pool[..schema.min.min(pool.len())].to_vec();
        let mut patches: Vec<(String, Value)> = Vec::new();
        if let Some(d) = registry.create(
            key,
            DrawingInit {
                anchors: complete.clone(),
                ..DrawingInit::on_pane(PANE)
            },
        ) {
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
        }
        for patch in patches {
            recipes.push(Recipe {
                anchors: complete.clone(),
                patch: Some(patch.clone()),
                ..Recipe::default()
            });
            recipes.push(Recipe {
                anchors: complete.clone(),
                patch: Some(patch),
                text: Some(label.clone()),
                placed: true,
                selected: true,
                ..Recipe::default()
            });
        }

        let scenarios: Vec<Value> = recipes
            .iter()
            .filter_map(|recipe| run(&registry, key, recipe, &proj, &theme))
            .collect();
        types.push(json!({ "type": key, "scenarios": scenarios }));
    }

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
        "theme": theme_json,
        "id": ID,
        "bars": bars_json,
        "types": types,
    });
    println!("{out}");
}
