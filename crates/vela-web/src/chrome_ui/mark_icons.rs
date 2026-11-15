//! Registry icons as canvas images for the mark lane (`MarkIconRaster.ts`): each (icon, ink,
//! size, dpr) is rasterized once by loading an SVG data URL into an `<img>`. A miss answers
//! `None` (the lane paints the bare token) and starts the load; when the image lands the
//! chrome repaints.
//!
//! The icon markup comes from this crate's table ([`super::icons`], extensible through
//! [`register_icon`](super::register_icon)); an id nothing registered stays bare.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use vela_core::render::canvas::CanvasImage;
use vela_core::render::chrome::marks_paint::MarkIcons;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::ChromeUi;
use super::icons::markup;
use crate::dom::{Listener, attr, create_element, on};

#[wasm_bindgen(inline_js = r#"
// The registry's markup is written for `innerHTML` (duplicate attributes, no namespace),
// which a standalone SVG image would refuse: HTML-parse it, then XML-serialize it.
export function vela_standalone_svg(doc, markup, ink, px) {
    const tpl = doc.createElement('template');
    tpl.innerHTML = markup;
    const root = tpl.content ? tpl.content.firstElementChild : tpl.firstElementChild;
    if (!root || String(root.tagName).toLowerCase() !== 'svg') return undefined;
    root.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
    root.setAttribute('width', String(px));
    root.setAttribute('height', String(px));
    // What `currentColor` resolves to in a standalone image.
    root.setAttribute('color', ink);
    const XS = globalThis.XMLSerializer;
    if (typeof XS !== 'function') return undefined;
    return new XS().serializeToString(root);
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = vela_standalone_svg)]
    fn vela_standalone_svg(
        doc: &web_sys::Document,
        markup: &str,
        ink: &str,
        px: f64,
    ) -> Option<String>;
}

/// `markup` as a standalone SVG sized `px`, inked `ink`; `None` where it does not parse as
/// an `<svg>` or the document cannot serialize XML.
fn standalone_svg(doc: &web_sys::Document, markup: &str, ink: &str, px: f64) -> Option<String> {
    vela_standalone_svg(doc, markup, ink, px)
}

enum Entry {
    Loading(#[allow(dead_code)] Vec<Listener>),
    Ready(CanvasImage),
    Failed,
}

/// The mark lane's icon source.
pub struct BrowserMarkIcons {
    ui: Weak<ChromeUi>,
    cache: RefCell<HashMap<(String, String, i64, i64), Entry>>,
}

impl BrowserMarkIcons {
    fn new(ui: Weak<ChromeUi>) -> Rc<Self> {
        Rc::new(Self {
            ui,
            cache: RefCell::new(HashMap::new()),
        })
    }
}

impl MarkIcons for BrowserMarkIcons {
    fn get(&self, icon: &str, ink: &str, px: f64, dpr: f64) -> Option<CanvasImage> {
        let key = (
            icon.to_string(),
            ink.to_string(),
            px.round() as i64,
            (dpr * 100.0).round() as i64,
        );
        if let Some(entry) = self.cache.borrow().get(&key) {
            return match entry {
                Entry::Ready(image) => Some(image.clone()),
                _ => None,
            };
        }
        let Some(svg) = markup(icon) else {
            self.cache.borrow_mut().insert(key, Entry::Failed);
            return None;
        };
        let ui = self.ui.upgrade()?;
        let size = (px * dpr).ceil().max(1.0);
        let Some(svg) = standalone_svg(&ui.doc, &svg, ink, size) else {
            self.cache.borrow_mut().insert(key, Entry::Failed);
            return None;
        };
        let url = format!(
            "data:image/svg+xml;charset=utf-8,{}",
            String::from(js_sys::encode_uri_component(&svg))
        );
        let img: Element = create_element(&ui.doc, "img");
        let mut listeners = Vec::new();
        {
            let weak = self.ui.clone();
            let key = key.clone();
            let img2 = img.clone();
            listeners.push(on(&img, "load", move |_ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                let image: CanvasImage = Rc::new(JsValue::from(img2.clone()));
                let old = ui.mark_icons.borrow().as_ref().and_then(|i| {
                    i.cache
                        .borrow_mut()
                        .insert(key.clone(), Entry::Ready(image))
                });
                if let Some(old) = old {
                    ui.retire(Box::new(old)); // the load handler may be the one running
                }
                if let Some(r) = ui.renderer() {
                    r.invalidate_chrome();
                }
            }));
        }
        {
            let weak = self.ui.clone();
            let key = key.clone();
            listeners.push(on(&img, "error", move |_ev: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                let old = ui
                    .mark_icons
                    .borrow()
                    .as_ref()
                    .and_then(|i| i.cache.borrow_mut().insert(key.clone(), Entry::Failed));
                if let Some(old) = old {
                    ui.retire(Box::new(old));
                }
            }));
        }
        self.cache
            .borrow_mut()
            .insert(key, Entry::Loading(listeners));
        attr(&img, "src", &url);
        None
    }
}

impl ChromeUi {
    /// The mark lane's icon source (one per chart).
    pub fn mark_icons(&self) -> Rc<BrowserMarkIcons> {
        let mut slot = self.mark_icons.borrow_mut();
        slot.get_or_insert_with(|| BrowserMarkIcons::new(self.me.clone()))
            .clone()
    }
}
