//! Shell option DEFAULTS contributed by plugins. A plugin cannot reach the options a host
//! builds its shell with, yet some of what it adds needs a shell built a certain way (a
//! sidebar for its entries). The shell-side counterpart of the renderer defaults: the same
//! shape, read once by every shell built afterwards, the widget's and a workspace.
//!
//! The original keeps this in a module global; here a [`ShellDefaultsRegistry`] is the
//! explicit value the caller carries (clones share it).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::sidebar_composition::SidebarComposition;
use crate::runtime::Unsubscribe;

/// The `sidebar` shell option: `true` / `false`, or a composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarOption {
    Enabled(bool),
    Composition(SidebarComposition),
}

/// The shell options a plugin may give a default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellDefaults {
    /// The right-edge toolbar (`sidebar`), as a shell option takes it.
    pub sidebar: Option<SidebarOption>,
}

#[derive(Default)]
struct Slot {
    value: Option<(u64, SidebarOption)>,
}

#[derive(Default)]
struct State {
    sidebar: Slot,
    next_generation: Cell<u64>,
}

/// The registry of shell option defaults. Clones share it.
#[derive(Clone, Default)]
pub struct ShellDefaultsRegistry {
    state: Rc<RefCell<State>>,
}

impl ShellDefaultsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register defaults for shell options. Every shell built afterwards uses one where
    /// its host gave no value for that option: the host's own value, `false` included,
    /// always wins. Shells already built are untouched. Returns a disposer that removes
    /// precisely the values it set (leaving any later re-registration of the same key in
    /// place).
    pub fn register(&self, values: ShellDefaults) -> Unsubscribe {
        let mut applied_sidebar: Option<u64> = None;
        if let Some(sidebar) = values.sidebar {
            let mut st = self.state.borrow_mut();
            let generation = st.next_generation.get();
            st.next_generation.set(generation + 1);
            st.sidebar.value = Some((generation, sidebar));
            applied_sidebar = Some(generation);
        }
        let state = Rc::downgrade(&self.state);
        Box::new(move || {
            let Some(state) = state.upgrade() else {
                return;
            };
            let mut st = state.borrow_mut();
            if let Some(generation) = applied_sidebar
                && st.sidebar.value.as_ref().map(|(g, _)| *g) == Some(generation)
            {
                st.sidebar.value = None;
            }
        })
    }

    /// Drop registered defaults: the named keys, or all of them when `keys` is empty.
    pub fn unregister(&self, keys: &[ShellDefaultKey]) {
        let mut st = self.state.borrow_mut();
        if keys.is_empty() || keys.contains(&ShellDefaultKey::Sidebar) {
            st.sidebar.value = None;
        }
    }

    /// The defaults a shell applies at construction (empty when no plugin registered any).
    pub fn defaults(&self) -> ShellDefaults {
        ShellDefaults {
            sidebar: self
                .state
                .borrow()
                .sidebar
                .value
                .as_ref()
                .map(|(_, v)| v.clone()),
        }
    }
}

/// The keys of [`ShellDefaults`], for [`ShellDefaultsRegistry::unregister`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellDefaultKey {
    Sidebar,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidebar(v: SidebarOption) -> ShellDefaults {
        ShellDefaults { sidebar: Some(v) }
    }

    #[test]
    fn an_empty_registry_has_no_defaults() {
        assert_eq!(
            ShellDefaultsRegistry::new().defaults(),
            ShellDefaults::default()
        );
    }

    #[test]
    fn a_registered_default_reaches_shells_built_afterwards_only() {
        let reg = ShellDefaultsRegistry::new();
        let before = reg.defaults();
        let _off = reg.register(sidebar(SidebarOption::Enabled(true)));
        let after = reg.defaults();
        assert_eq!(before.sidebar, None);
        assert_eq!(after.sidebar, Some(SidebarOption::Enabled(true)));
    }

    #[test]
    fn a_composition_registers_like_the_option_takes_it() {
        let reg = ShellDefaultsRegistry::new();
        let comp = SidebarComposition {
            top: Some(vec!["panels".into()]),
            bottom: Some(vec![]),
        };
        let _off = reg.register(sidebar(SidebarOption::Composition(comp.clone())));
        assert_eq!(
            reg.defaults().sidebar,
            Some(SidebarOption::Composition(comp))
        );
    }

    #[test]
    fn the_disposer_removes_only_its_own_value() {
        let reg = ShellDefaultsRegistry::new();
        let off = reg.register(sidebar(SidebarOption::Composition(SidebarComposition {
            top: Some(vec!["panels".into()]),
            bottom: Some(vec![]),
        })));
        let later = reg.register(sidebar(SidebarOption::Enabled(true)));
        off();
        assert_eq!(
            reg.defaults(),
            ShellDefaults {
                sidebar: Some(SidebarOption::Enabled(true))
            }
        );
        later();
        assert_eq!(reg.defaults(), ShellDefaults::default());
    }

    #[test]
    fn unregister_drops_named_keys_or_everything() {
        let reg = ShellDefaultsRegistry::new();
        let _a = reg.register(sidebar(SidebarOption::Enabled(true)));
        reg.unregister(&[ShellDefaultKey::Sidebar]);
        assert_eq!(reg.defaults().sidebar, None);
        let _b = reg.register(sidebar(SidebarOption::Enabled(false)));
        reg.unregister(&[]);
        assert_eq!(reg.defaults().sidebar, None);
    }

    #[test]
    fn registering_nothing_leaves_an_earlier_value_and_its_disposer_is_inert() {
        let reg = ShellDefaultsRegistry::new();
        let _a = reg.register(sidebar(SidebarOption::Enabled(true)));
        let noop = reg.register(ShellDefaults::default());
        noop();
        assert_eq!(reg.defaults().sidebar, Some(SidebarOption::Enabled(true)));
    }
}
