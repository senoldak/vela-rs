//! The inline text editor of a text drawing: one absolutely positioned `<textarea>` over
//! the plot, built from a [`TextEditorSpec`] exactly as `user_controller::ui` documents it.
//!
//! `Open` creates it (replacing any open one), sets its geometry and look, puts the caret at
//! the END with nothing selected and focuses it; `Layout` re-applies geometry and look (the
//! live value stays the textarea's own); `Close` removes it. The textarea stops `pointerdown`
//! and `keydown` propagation, reports every `input` as `text_editor_input`, inserts a line
//! break on a plain Enter itself, commits on Ctrl/Cmd+Enter, cancels on Escape, and reports
//! `blur` with whether focus moved into the settings bar.

use vela_core::render::drawings::{TextEditorEvent, TextEditorSpec};
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::DrawingsUi;
use crate::dom::{
    Listener, attr, get_bool, get_prop, get_str, make, on, prevent_default, set_styles,
    stop_propagation, vela_caret_end, vela_focus, vela_get_value, vela_insert_newline,
    vela_set_value,
};

pub struct Editor {
    el: Element,
    listeners: Vec<Listener>,
}

impl Editor {
    /// Remove the element and free the listeners now (the host is going away; no handler
    /// of ours can be running).
    pub fn dispose(self) {
        self.el.remove();
        drop(self.listeners);
    }
}

fn px(v: f64) -> String {
    format!("{v}px")
}

/// Apply the spec's geometry and look.
fn style(el: &Element, spec: &TextEditorSpec) {
    let border = if spec.border_width > 0.0 {
        format!("{}px solid {}", spec.border_width, spec.border_color)
    } else {
        "none".to_string()
    };
    let padding = format!("{}px {}px", spec.padding.0, spec.padding.1);
    let line_height = spec.line_height.map(px);
    let styles: Vec<(&str, String)> = vec![
        ("position", "absolute".into()),
        ("z-index", "24".into()),
        ("box-sizing", "border-box".into()),
        ("resize", "none".into()),
        ("overflow", "hidden".into()),
        ("margin", "0".into()),
        ("outline", "none".into()),
        ("pointer-events", "auto".into()),
        ("text-align", "left".into()),
        ("left", px(spec.x)),
        ("top", px(spec.y)),
        ("width", px(spec.width)),
        ("height", px(spec.height)),
        ("font", spec.font.clone()),
        ("color", spec.color.clone()),
        ("background", spec.background.clone()),
        ("border", border),
        ("border-radius", px(spec.border_radius)),
        ("padding", padding),
        (
            "white-space",
            if spec.preserve_whitespace { "pre" } else { "" }.into(),
        ),
        ("line-height", line_height.unwrap_or_default()),
    ];
    // An empty value clears the property (a label's `white-space` / `line-height` only).
    let borrowed: Vec<(&str, &str)> = styles.iter().map(|(k, v)| (*k, v.as_str())).collect();
    set_styles(el, &borrowed);
}

impl DrawingsUi {
    pub(super) fn editor_event(&self, event: &TextEditorEvent) {
        match event {
            TextEditorEvent::Open(spec) => self.editor_open(spec),
            TextEditorEvent::Layout(spec) => {
                if let Some(editor) = self.editor.borrow().as_ref() {
                    style(&editor.el, spec);
                }
            }
            TextEditorEvent::Close => self.editor_close(),
        }
    }

    fn editor_close(&self) {
        let editor = self.editor.borrow_mut().take();
        let Some(editor) = editor else { return };
        // Free the handlers first: removing a focused element can fire `blur`, and a blur
        // report here would re-enter the controller that is closing the editor.
        let Editor { el, listeners } = editor;
        self.retire(listeners);
        el.remove();
    }

    fn editor_open(&self, spec: &TextEditorSpec) {
        self.editor_close();
        let el = make(&self.doc, "textarea", "vela-dtext");
        attr(&el, "spellcheck", "false");
        attr(&el, "placeholder", &spec.placeholder);
        attr(&el, "aria-label", &spec.placeholder);
        style(&el, spec);
        vela_set_value(&el, &spec.text);
        let _ = self.plot.append_child(&el);

        let mut listeners = Vec::new();
        // A press inside must not reach the chart (it would end the edit or start a pan).
        listeners.push(on(&el, "pointerdown", |ev: JsValue| stop_propagation(&ev)));
        {
            let ui = self.weak();
            let target = el.clone();
            listeners.push(on(&el, "keydown", move |ev: JsValue| {
                stop_propagation(&ev);
                if get_bool(&ev, "isComposing") {
                    return; // an IME owns Enter and Escape while composing
                }
                let key = get_str(&ev, "key");
                let modifier = get_bool(&ev, "ctrlKey") || get_bool(&ev, "metaKey");
                match key.as_str() {
                    "Enter" if modifier => {
                        prevent_default(&ev);
                        DrawingsUi::with_ctl(&ui, |c| c.text_editor_commit());
                    }
                    "Enter" => {
                        prevent_default(&ev);
                        vela_insert_newline(&target);
                        let value = vela_get_value(&target);
                        DrawingsUi::with_ctl(&ui, |c| c.text_editor_input(&value));
                    }
                    "Escape" => {
                        prevent_default(&ev);
                        DrawingsUi::with_ctl(&ui, |c| c.text_editor_cancel());
                    }
                    _ => {}
                }
            }));
        }
        {
            let ui = self.weak();
            let target = el.clone();
            listeners.push(on(&el, "input", move |_| {
                let value = vela_get_value(&target);
                DrawingsUi::with_ctl(&ui, |c| c.text_editor_input(&value));
            }));
        }
        {
            let ui = self.weak();
            listeners.push(on(&el, "blur", move |ev: JsValue| {
                let into_popup = ui
                    .upgrade()
                    .is_some_and(|u| u.popup_contains(&get_prop(&ev, "relatedTarget")));
                DrawingsUi::with_ctl(&ui, |c| c.text_editor_blur(into_popup));
            }));
        }
        *self.editor.borrow_mut() = Some(Editor {
            el: el.clone(),
            listeners,
        });
        vela_focus(&el);
        vela_caret_end(&el);
    }
}
