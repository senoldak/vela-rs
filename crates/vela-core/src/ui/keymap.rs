//! `KeymapManager`: the central, descriptor-based shortcut system. Bindings are data
//! (consistent with the plugin contribution model): declarative descriptors with ids,
//! human labels (queryable for a shortcuts help panel), scopes, and rebindable keys.
//!
//! Key strings: `"mod+shift+k"`, `"alt"`, `"arrowleft"`, `"ctrl+="`, `"f2"`, `"space"`.
//! - `mod` is the command key on macOS and Ctrl elsewhere (normalized at match time).
//! - Modifiers are order-insensitive; the final token is the key (`KeyboardEvent.key`,
//!   lowercased; aliases: esc/space/plus/minus).
//!
//! Scopes are a stack ([`KeymapManager::push_scope`] / [`KeymapManager::pop_scope`]). A
//! binding fires when its scope is the top of the stack, or always when its scope is
//! `"global"`. Opening a dialog pushes `"dialog"` and thereby mutes `"chart"` bindings
//! without unregistering anything.
//!
//! Keyboard layouts: a chord names Latin keys. A layout that types another script there
//! (Cyrillic `я` on the Z key) still fires `mod+z`: the match falls back to the physical key.
//!
//! The key event is plain data ([`KeyEvent`]); `preventDefault` / `stopPropagation` are
//! flags the browser crate reads back after the match. Attaching to a DOM target (`attach`,
//! `detach`) and sniffing `navigator.platform` stay with the browser: pass the platform in
//! [`KeymapOptions`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::{MessageLike, Translator, msg, text_of};
use crate::runtime::Unsubscribe;

/// The element a keystroke landed on, as far as the matcher cares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyEventTarget {
    /// `tagName` as the DOM reports it (any case).
    pub tag_name: String,
    pub is_content_editable: bool,
    /// The `role` attribute.
    pub role: Option<String>,
}

/// A `keydown` as plain data.
#[derive(Clone, Debug, Default)]
pub struct KeyEvent {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code` (the physical key, e.g. `KeyZ`).
    pub code: String,
    pub ctrl: bool,
    pub meta: bool,
    pub alt: bool,
    pub shift: bool,
    pub target: Option<KeyEventTarget>,
    /// Set when a matched binding asks for `preventDefault`.
    pub default_prevented: Cell<bool>,
    /// Set when a matched binding asks for `stopPropagation`.
    pub propagation_stopped: Cell<bool>,
}

impl KeyEvent {
    /// An event for `key` with no modifiers and no target.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }
}

/// A shortcut binding.
#[derive(Clone)]
pub struct KeyBindingDescriptor {
    /// Stable id, e.g. `"chart.toggle-log-scale"`. Re-registering an id replaces it.
    pub id: String,
    /// Default chord(s). Users can [`rebind`](KeymapManager::rebind) without touching the descriptor.
    pub keys: Vec<String>,
    /// Human label for the shortcuts help panel.
    pub label: MessageLike,
    /// Help-panel grouping, e.g. `"Chart"`, `"Drawings"`.
    pub category: Option<MessageLike>,
    /// `"global"` fires in any scope; anything else only while it is the top scope.
    /// Default: the manager's base scope (`"chart"`).
    pub scope: Option<String>,
    /// Extra runtime gate evaluated at match time.
    pub when: Option<Rc<dyn Fn() -> bool>>,
    /// Fire even while an input/textarea/contenteditable has focus (default false).
    pub allow_in_input: bool,
    /// `preventDefault` + `stopPropagation` on match (default true).
    pub prevent_default: bool,
    pub run: Rc<dyn Fn(&KeyEvent)>,
}

impl KeyBindingDescriptor {
    /// A binding with one chord and the original's defaults for everything else.
    pub fn new(
        id: impl Into<String>,
        keys: impl Into<String>,
        label: impl Into<MessageLike>,
        run: impl Fn(&KeyEvent) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            keys: vec![keys.into()],
            label: label.into(),
            category: None,
            scope: None,
            when: None,
            allow_in_input: false,
            prevent_default: true,
            run: Rc::new(run),
        }
    }
}

/// A binding as the help panel / rebinding UI sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub id: String,
    pub label: String,
    pub category: String,
    pub scope: String,
    /// Active chords (custom rebinds if present, else descriptor defaults).
    pub keys: Vec<String>,
    /// Display form per platform, e.g. `⇧⌘K` (mac) / `Ctrl+Shift+K`.
    pub display: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapPlatform {
    Mac,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct KeymapOptions {
    /// The platform (the original sniffs `navigator.platform` when unset; here unset is
    /// [`KeymapPlatform::Other`]).
    pub platform: Option<KeymapPlatform>,
    /// The scope active when the stack is empty. Default `"chart"`.
    pub base_scope: Option<String>,
}

#[derive(Default)]
struct Chord {
    ctrl: bool,
    meta: bool,
    alt: bool,
    shift: bool,
    key: String,
}

fn key_alias(p: &str) -> Option<&'static str> {
    Some(match p {
        "esc" => "escape",
        "space" => " ",
        "plus" => "+",
        "minus" => "-",
        "del" => "delete",
        "return" => "enter",
        "left" => "arrowleft",
        "right" => "arrowright",
        "up" => "arrowup",
        "down" => "arrowdown",
        _ => return None,
    })
}

/// JavaScript's `string.length` (UTF-16 code units).
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn parse_chord(spec: &str, mac: bool) -> Chord {
    let lowered = spec.to_lowercase();
    let all: Vec<&str> = lowered.split('+').map(str::trim).collect();
    // Keep one empty piece from "ctrl++": it names the key "+".
    let parts: Vec<&str> = all
        .iter()
        .enumerate()
        .filter(|(i, p)| !p.is_empty() || (*i > 0 && all[*i - 1].is_empty()))
        .map(|(_, p)| *p)
        .collect();
    let mut chord = Chord::default();
    for raw in parts {
        let p = if raw.is_empty() { "+" } else { raw };
        match p {
            "mod" if mac => chord.meta = true,
            "mod" => chord.ctrl = true,
            "ctrl" | "control" => chord.ctrl = true,
            "meta" | "cmd" | "command" => chord.meta = true,
            "alt" | "option" => chord.alt = true,
            "shift" => chord.shift = true,
            _ => chord.key = key_alias(p).unwrap_or(p).to_string(),
        }
    }
    chord
}

fn is_ascii_lower_or_digit(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// The key a keystroke names, as chords spell it: `key` lowercased, unless the layout
/// typed a letter of another script there; then the physical key's Latin letter or digit
/// (`KeyZ` becomes `z`), so shortcuts keep working on Cyrillic, Greek, Arabic or Hebrew
/// layouts. A Latin layout keeps its own letters (AZERTY's Z stays `z` wherever it sits).
pub fn chord_key_of(key: &str, code: &str) -> String {
    let key = key.to_lowercase();
    let mut chars = key.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(c), None) if js_len(&key) == 1 => Some(c),
        _ => None,
    };
    let Some(c) = single else {
        return key;
    };
    if is_ascii_lower_or_digit(c) || !c.is_alphabetic() {
        return key;
    }
    let physical = code
        .strip_prefix("Key")
        .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_uppercase())
        .or_else(|| {
            code.strip_prefix("Digit")
                .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit())
        });
    match physical {
        Some(p) => p.to_lowercase(),
        None => key,
    }
}

fn event_matches(ev: &KeyEvent, c: &Chord) -> bool {
    // Shift is part of producing many printable keys ('?', '+'): only enforce it when the
    // chord names a non-printable / letter key where shift is a real modifier.
    let shift_ok = if js_len(&c.key) > 1 || is_plain_key(&c.key) {
        ev.shift == c.shift
    } else {
        true
    };
    ev.ctrl == c.ctrl
        && ev.meta == c.meta
        && ev.alt == c.alt
        && shift_ok
        && chord_key_of(&ev.key, &ev.code) == c.key
}

fn is_plain_key(key: &str) -> bool {
    let mut it = key.chars();
    matches!((it.next(), it.next()), (Some(c), None) if is_ascii_lower_or_digit(c) || c == ' ')
}

/// Whether a keystroke is being typed somewhere, in which case the chart's single-letter
/// shortcuts must not fire. Three signals, because "a thing that accepts text" has three
/// spellings on the platform: a form control, a contenteditable, and (since the
/// EditContext API) a plain element that merely declares `role="textbox"`. Modern code
/// editors use exactly that third form, so recognising only the first two would turn
/// every letter typed there into a chart shortcut.
pub fn is_editable_target(ev: &KeyEvent) -> bool {
    let Some(t) = &ev.target else {
        return false;
    };
    let tag = t.tag_name.to_lowercase();
    if tag == "input" || tag == "textarea" || tag == "select" {
        return true;
    }
    t.is_content_editable || t.role.as_deref() == Some("textbox")
}

fn display_chord(spec: &str, mac: bool, i18n: Option<&dyn Translator>) -> String {
    let c = parse_chord(spec, mac);
    let name = |m: MessageLike| text_of(i18n, &m, &[]);
    let key_label = if c.key == " " {
        name(msg("ui.keys.space", "Space"))
    } else if js_len(&c.key) == 1 {
        c.key.to_uppercase()
    } else {
        let mut chars = c.key.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    };
    if mac {
        let mut out = String::new();
        if c.ctrl {
            out.push('\u{2303}');
        }
        if c.alt {
            out.push('\u{2325}');
        }
        if c.shift {
            out.push('\u{21e7}');
        }
        if c.meta {
            out.push('\u{2318}');
        }
        out.push_str(&key_label);
        return out;
    }
    let mut mods: Vec<String> = Vec::new();
    if c.ctrl {
        mods.push(name(msg("ui.keys.ctrl", "Ctrl")));
    }
    if c.alt {
        mods.push(name(msg("ui.keys.alt", "Alt")));
    }
    if c.shift {
        mods.push(name(msg("ui.keys.shift", "Shift")));
    }
    if c.meta {
        mods.push(name(msg("ui.keys.meta", "Win")));
    }
    mods.push(key_label);
    mods.join("+")
}

#[derive(Default)]
struct State {
    /// Registration order; replacing an id keeps its place, as a `Map` does.
    descriptors: Vec<Rc<KeyBindingDescriptor>>,
    rebinds: Vec<(String, Vec<String>)>,
    scope_stack: Vec<String>,
}

/// The central shortcut registry and matcher.
pub struct KeymapManager {
    mac: bool,
    base_scope: String,
    state: Rc<RefCell<State>>,
}

impl KeymapManager {
    pub fn new(opts: KeymapOptions) -> Self {
        Self {
            mac: opts.platform == Some(KeymapPlatform::Mac),
            base_scope: opts.base_scope.unwrap_or_else(|| "chart".to_string()),
            state: Rc::new(RefCell::new(State::default())),
        }
    }

    /// Register (or replace, by id) a binding. Returns a disposer that removes only this
    /// registration (a stale disposer never removes a later one).
    pub fn register(&self, desc: KeyBindingDescriptor) -> Unsubscribe {
        let desc = Rc::new(desc);
        {
            let mut st = self.state.borrow_mut();
            match st.descriptors.iter_mut().find(|d| d.id == desc.id) {
                Some(slot) => *slot = Rc::clone(&desc),
                None => st.descriptors.push(Rc::clone(&desc)),
            }
        }
        let state = Rc::downgrade(&self.state);
        Box::new(move || {
            if let Some(state) = state.upgrade() {
                state
                    .borrow_mut()
                    .descriptors
                    .retain(|d| !Rc::ptr_eq(d, &desc));
            }
        })
    }

    pub fn unregister(&self, id: &str) {
        let mut st = self.state.borrow_mut();
        st.descriptors.retain(|d| d.id != id);
        st.rebinds.retain(|(k, _)| k != id);
    }

    /// User-level rebinding: overrides the descriptor's default chords (`None` resets).
    pub fn rebind(&self, id: &str, keys: Option<Vec<String>>) {
        let mut st = self.state.borrow_mut();
        st.rebinds.retain(|(k, _)| k != id);
        if let Some(keys) = keys {
            st.rebinds.push((id.to_string(), keys));
        }
    }

    /// Snapshot for a shortcuts help panel / rebinding UI: labels, categories and key
    /// names in `i18n`'s language (English when absent).
    pub fn bindings(&self, i18n: Option<&dyn Translator>) -> Vec<ResolvedBinding> {
        let st = self.state.borrow();
        st.descriptors
            .iter()
            .map(|d| {
                let keys = Self::active_keys(&st, d);
                ResolvedBinding {
                    id: d.id.clone(),
                    label: text_of(i18n, &d.label, &[]),
                    category: text_of(
                        i18n,
                        d.category
                            .as_ref()
                            .unwrap_or(&msg("ui.keys.general", "General")),
                        &[],
                    ),
                    scope: d.scope.clone().unwrap_or_else(|| self.base_scope.clone()),
                    display: keys
                        .iter()
                        .map(|k| display_chord(k, self.mac, i18n))
                        .collect(),
                    keys,
                }
            })
            .collect()
    }

    pub fn push_scope(&self, scope: &str) {
        self.state.borrow_mut().scope_stack.push(scope.to_string());
    }

    /// Pops the topmost occurrence of `scope` (tolerates out-of-order teardown).
    pub fn pop_scope(&self, scope: &str) {
        let mut st = self.state.borrow_mut();
        if let Some(i) = st.scope_stack.iter().rposition(|s| s == scope) {
            st.scope_stack.remove(i);
        }
    }

    pub fn active_scope(&self) -> String {
        self.state
            .borrow()
            .scope_stack
            .last()
            .cloned()
            .unwrap_or_else(|| self.base_scope.clone())
    }

    /// The matcher: public so hosts and tests can feed events from their own listeners.
    /// Returns whether a binding handled the event.
    pub fn handle_keydown(&self, ev: &KeyEvent) -> bool {
        let editable = is_editable_target(ev);
        let active_scope = self.active_scope();
        // Snapshot: a handler may register, rebind or push a scope while it runs.
        let snapshot: Vec<(Rc<KeyBindingDescriptor>, Vec<String>)> = {
            let st = self.state.borrow();
            st.descriptors
                .iter()
                .map(|d| (Rc::clone(d), Self::active_keys(&st, d)))
                .collect()
        };
        for (d, keys) in snapshot {
            let scope = d.scope.as_deref().unwrap_or(&self.base_scope);
            if scope != "global" && scope != active_scope {
                continue;
            }
            if editable && !d.allow_in_input {
                continue;
            }
            if let Some(when) = &d.when
                && !when()
            {
                continue;
            }
            for spec in &keys {
                if event_matches(ev, &parse_chord(spec, self.mac)) {
                    if d.prevent_default {
                        ev.default_prevented.set(true);
                        ev.propagation_stopped.set(true);
                    }
                    (d.run)(ev);
                    return true;
                }
            }
        }
        false
    }

    pub fn destroy(&self) {
        let mut st = self.state.borrow_mut();
        st.descriptors.clear();
        st.rebinds.clear();
        st.scope_stack.clear();
    }

    fn active_keys(st: &State, d: &KeyBindingDescriptor) -> Vec<String> {
        st.rebinds
            .iter()
            .find(|(k, _)| *k == d.id)
            .map(|(_, keys)| keys.clone())
            .unwrap_or_else(|| d.keys.clone())
    }
}

impl Default for KeymapManager {
    fn default() -> Self {
        Self::new(KeymapOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{LocalePack, create_translator};
    use std::sync::Arc;

    fn ev(key: &str) -> KeyEvent {
        KeyEvent::new(key)
    }

    fn counter() -> (Rc<Cell<u32>>, impl Fn(&KeyEvent) + 'static) {
        let n = Rc::new(Cell::new(0));
        let n2 = Rc::clone(&n);
        (n, move |_| n2.set(n2.get() + 1))
    }

    fn manager(platform: KeymapPlatform) -> KeymapManager {
        KeymapManager::new(KeymapOptions {
            platform: Some(platform),
            base_scope: None,
        })
    }

    #[test]
    fn matches_chords_with_order_insensitive_modifiers_and_key_aliases() {
        let km = manager(KeymapPlatform::Other);
        let (n, run) = counter();
        let _ = km.register(KeyBindingDescriptor::new("t", "shift+ctrl+k", "T", run));
        let e = KeyEvent {
            ctrl: true,
            shift: true,
            ..ev("K")
        };
        assert!(km.handle_keydown(&e));
        let e = KeyEvent {
            ctrl: true,
            ..ev("k")
        };
        assert!(!km.handle_keydown(&e)); // shift required
        let (n2, run2) = counter();
        let _ = km.register(KeyBindingDescriptor::new("esc", "esc", "E", run2));
        assert!(km.handle_keydown(&ev("Escape")));
        assert_eq!(n.get() + n2.get(), 2);
    }

    #[test]
    fn mod_resolves_to_meta_on_mac_and_ctrl_elsewhere() {
        let mac = manager(KeymapPlatform::Mac);
        let _ = mac.register(KeyBindingDescriptor::new("z", "mod+z", "Undo", |_| {}));
        let meta = KeyEvent {
            meta: true,
            ..ev("z")
        };
        let ctrl = KeyEvent {
            ctrl: true,
            ..ev("z")
        };
        assert!(mac.handle_keydown(&meta));
        assert!(!mac.handle_keydown(&ctrl));

        let win = manager(KeymapPlatform::Other);
        let _ = win.register(KeyBindingDescriptor::new("z", "mod+z", "Undo", |_| {}));
        assert!(win.handle_keydown(&ctrl));
        assert!(!win.handle_keydown(&meta));
    }

    #[test]
    fn shift_is_tolerated_for_printable_punctuation_keys() {
        let km = manager(KeymapPlatform::Other);
        let _ = km.register(KeyBindingDescriptor::new("help", "?", "Help", |_| {}));
        let e = KeyEvent {
            shift: true,
            ..ev("?")
        };
        assert!(km.handle_keydown(&e));
    }

    #[test]
    fn scope_stack_top_scope_mutes_lower_scopes_and_global_always_fires() {
        let km = manager(KeymapPlatform::Other);
        let (chart, run_chart) = counter();
        let (dlg, run_dlg) = counter();
        let (glob, run_glob) = counter();
        let _ = km.register(KeyBindingDescriptor::new("c", "x", "C", run_chart));
        let mut d = KeyBindingDescriptor::new("d", "x", "D", run_dlg);
        d.scope = Some("dialog".into());
        let _ = km.register(d);
        let mut g = KeyBindingDescriptor::new("g", "g", "G", run_glob);
        g.scope = Some("global".into());
        let _ = km.register(g);

        km.handle_keydown(&ev("x"));
        assert_eq!((chart.get(), dlg.get()), (1, 0));

        km.push_scope("dialog");
        assert_eq!(km.active_scope(), "dialog");
        km.handle_keydown(&ev("x"));
        km.handle_keydown(&ev("g"));
        assert_eq!((chart.get(), dlg.get(), glob.get()), (1, 1, 1));

        km.pop_scope("dialog");
        km.handle_keydown(&ev("x"));
        assert_eq!(chart.get(), 2);
    }

    #[test]
    fn pop_scope_tolerates_out_of_order_and_unknown_scopes() {
        let km = manager(KeymapPlatform::Other);
        km.push_scope("a");
        km.push_scope("b");
        km.pop_scope("a");
        assert_eq!(km.active_scope(), "b");
        km.pop_scope("nope");
        assert_eq!(km.active_scope(), "b");
        km.pop_scope("b");
        assert_eq!(km.active_scope(), "chart");
    }

    fn target(tag: &str) -> Option<KeyEventTarget> {
        Some(KeyEventTarget {
            tag_name: tag.into(),
            ..KeyEventTarget::default()
        })
    }

    #[test]
    fn skips_editable_targets_unless_allow_in_input() {
        let km = manager(KeymapPlatform::Other);
        let _ = km.register(KeyBindingDescriptor::new("a", "a", "A", |_| {}));
        let mut b = KeyBindingDescriptor::new("b", "b", "B", |_| {});
        b.allow_in_input = true;
        let _ = km.register(b);
        let on_input = |key: &str| KeyEvent {
            target: target("INPUT"),
            ..ev(key)
        };
        assert!(!km.handle_keydown(&on_input("a")));
        assert!(km.handle_keydown(&on_input("b")));
    }

    #[test]
    fn treats_every_spelling_of_accepts_text_as_editable() {
        let km = manager(KeymapPlatform::Other);
        let _ = km.register(KeyBindingDescriptor::new("a", "a", "A", |_| {}));
        let role = |role: Option<&str>| {
            Some(KeyEventTarget {
                tag_name: "DIV".into(),
                is_content_editable: false,
                role: role.map(str::to_string),
            })
        };
        for t in [
            target("TEXTAREA"),
            target("SELECT"),
            Some(KeyEventTarget {
                tag_name: "DIV".into(),
                is_content_editable: true,
                role: None,
            }),
            role(Some("textbox")),
        ] {
            let e = KeyEvent {
                target: t,
                ..ev("a")
            };
            assert!(!km.handle_keydown(&e));
        }
        for t in [role(None), role(Some("button"))] {
            let e = KeyEvent {
                target: t,
                ..ev("a")
            };
            assert!(km.handle_keydown(&e));
        }
    }

    #[test]
    fn rebind_overrides_descriptor_keys_and_bindings_reflects_it() {
        let km = manager(KeymapPlatform::Other);
        let mut d = KeyBindingDescriptor::new("save", "mod+s", "Save layout", |_| {});
        d.category = Some("Chart".into());
        let _ = km.register(d);
        km.rebind("save", Some(vec!["mod+shift+s".into()]));
        let plain = KeyEvent {
            ctrl: true,
            ..ev("s")
        };
        let shifted = KeyEvent {
            ctrl: true,
            shift: true,
            ..ev("s")
        };
        assert!(!km.handle_keydown(&plain));
        assert!(km.handle_keydown(&shifted));

        let binding = km.bindings(None).into_iter().find(|b| b.id == "save");
        let b = binding.expect("binding");
        assert_eq!(b.keys, ["mod+shift+s"]);
        assert_eq!(b.display, ["Ctrl+Shift+S"]);
        assert_eq!(b.category, "Chart");

        km.rebind("save", None);
        assert!(km.handle_keydown(&plain));
    }

    #[test]
    fn mac_display_uses_glyphs() {
        let km = manager(KeymapPlatform::Mac);
        let _ = km.register(KeyBindingDescriptor::new("k", "mod+shift+k", "K", |_| {}));
        assert_eq!(km.bindings(None)[0].display[0], "\u{21e7}\u{2318}K");
    }

    #[test]
    fn prevent_default_defaults_on_and_can_be_opted_out_and_when_gates() {
        let km = manager(KeymapPlatform::Other);
        let enabled = Rc::new(Cell::new(false));
        let gate = Rc::clone(&enabled);
        let mut w = KeyBindingDescriptor::new("w", "w", "W", |_| {});
        w.when = Some(Rc::new(move || gate.get()));
        let _ = km.register(w);
        let e1 = ev("w");
        assert!(!km.handle_keydown(&e1));
        enabled.set(true);
        assert!(km.handle_keydown(&e1));
        assert!(e1.default_prevented.get());
        assert!(e1.propagation_stopped.get());

        let mut p = KeyBindingDescriptor::new("p", "p", "P", |_| {});
        p.prevent_default = false;
        let _ = km.register(p);
        let e2 = ev("p");
        km.handle_keydown(&e2);
        assert!(!e2.default_prevented.get());
    }

    #[test]
    fn re_registering_replaces_and_a_stale_disposer_only_removes_its_own() {
        let km = manager(KeymapPlatform::Other);
        let (first, run1) = counter();
        let (second, run2) = counter();
        let dispose1 = km.register(KeyBindingDescriptor::new("x", "x", "1", run1));
        let _ = km.register(KeyBindingDescriptor::new("x", "x", "2", run2));
        dispose1();
        assert!(km.handle_keydown(&ev("x")));
        assert_eq!((first.get(), second.get()), (0, 1));
    }

    #[test]
    fn a_handler_may_register_while_it_runs() {
        let km = Rc::new(manager(KeymapPlatform::Other));
        let inner = Rc::clone(&km);
        let _ = km.register(KeyBindingDescriptor::new("a", "a", "A", move |_| {
            let _ = inner.register(KeyBindingDescriptor::new("b", "b", "B", |_| {}));
        }));
        assert!(km.handle_keydown(&ev("a")));
        assert!(km.handle_keydown(&ev("b")));
    }

    #[test]
    fn chord_parsing_edge_cases() {
        let km = manager(KeymapPlatform::Other);
        let _ = km.register(KeyBindingDescriptor::new(
            "plus",
            "ctrl++",
            "Zoom in",
            |_| {},
        ));
        let e = KeyEvent {
            ctrl: true,
            ..ev("+")
        };
        assert!(km.handle_keydown(&e));
        let _ = km.register(KeyBindingDescriptor::new("sp", "space", "Play", |_| {}));
        assert!(km.handle_keydown(&ev(" ")));
        // An empty spec names no key: nothing matches the empty string.
        let _ = km.register(KeyBindingDescriptor::new("empty", "", "Nothing", |_| {}));
        assert_eq!(km.bindings(None).len(), 3);
    }

    fn press(key: &str, code: &str, ctrl: bool, alt: bool) -> KeyEvent {
        KeyEvent {
            key: key.into(),
            code: code.into(),
            ctrl,
            alt,
            ..KeyEvent::default()
        }
    }

    #[test]
    fn ctrl_z_still_undoes_on_a_cyrillic_layout() {
        let km = manager(KeymapPlatform::Other);
        let (n, run) = counter();
        let _ = km.register(KeyBindingDescriptor::new("undo", "mod+z", "Undo", run));
        assert!(km.handle_keydown(&press("\u{44f}", "KeyZ", true, false)));
        assert_eq!(n.get(), 1);
    }

    #[test]
    fn letter_shortcuts_reach_greek_hebrew_and_arabic_layouts() {
        let km = manager(KeymapPlatform::Other);
        let (n, run) = counter();
        let _ = km.register(KeyBindingDescriptor::new(
            "trend",
            "alt+t",
            "Trend line",
            run,
        ));
        km.handle_keydown(&press("\u{3c4}", "KeyT", false, true));
        km.handle_keydown(&press("\u{5d0}", "KeyT", false, true));
        km.handle_keydown(&press("\u{641}", "KeyT", false, true));
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn a_latin_layout_keeps_its_own_letters_wherever_they_sit() {
        let km = manager(KeymapPlatform::Other);
        let (n, run) = counter();
        let _ = km.register(KeyBindingDescriptor::new("undo", "mod+z", "Undo", run));
        // On AZERTY the Z sits where QWERTY has W, and the reverse.
        assert!(km.handle_keydown(&press("z", "KeyW", true, false)));
        assert!(!km.handle_keydown(&press("w", "KeyZ", true, false)));
        assert_eq!(n.get(), 1);
    }

    #[test]
    fn chord_key_of_ignores_unusable_physical_codes() {
        assert_eq!(chord_key_of("\u{44f}", "Space"), "\u{44f}");
        assert_eq!(chord_key_of("\u{44f}", ""), "\u{44f}");
        assert_eq!(chord_key_of("\u{44f}", "Digit5"), "5");
        assert_eq!(chord_key_of("Escape", "Escape"), "escape");
        assert_eq!(chord_key_of("?", "Slash"), "?");
    }

    #[test]
    fn the_shortcuts_panel_names_the_keys_in_the_shells_language() {
        use crate::i18n::{LocaleRegistry, LocalizationOptions};
        use std::collections::HashMap;
        let intl = crate::intl::default_intl();
        let registry = LocaleRegistry::new(Arc::clone(&intl));
        let messages: HashMap<String, String> = [
            ("ui.keys.ctrl", "Strg"),
            ("ui.keys.shift", "Umschalt"),
            ("ui.keys.space", "Leertaste"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let _registration = registry.register(&LocalePack {
            locale: "de".into(),
            direction: None,
            messages: Arc::new(messages),
        });
        let de = create_translator(
            LocalizationOptions {
                locale: Some("de".into()),
                ..LocalizationOptions::default()
            },
            intl,
            registry,
        );
        let km = manager(KeymapPlatform::Other);
        let _ = km.register(KeyBindingDescriptor::new(
            "redo",
            "mod+shift+z",
            "Redo",
            |_| {},
        ));
        let _ = km.register(KeyBindingDescriptor::new("play", "space", "Play", |_| {}));
        let shown =
            |b: Vec<ResolvedBinding>| b.iter().map(|b| b.display[0].clone()).collect::<Vec<_>>();
        assert_eq!(shown(km.bindings(None)), ["Ctrl+Shift+Z", "Space"]);
        assert_eq!(
            shown(km.bindings(Some(&de))),
            ["Strg+Umschalt+Z", "Leertaste"]
        );
    }
}
