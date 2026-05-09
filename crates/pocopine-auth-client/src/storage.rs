//! Pluggable bearer-token persistence.
//!
//! By default the token slot is in-memory only — page reloads sign the
//! user out. Real apps persist the token through some browser surface
//! (`localStorage`, `sessionStorage`, an `httpOnly` cookie set by the
//! server, IndexedDB, …). Each option has tradeoffs around durability,
//! cross-tab visibility, and XSS exposure.
//!
//! `pocopine-auth-client` ships [`InMemory`] (the no-op default) plus
//! [`LocalStorage`] and [`SessionStorage`] for the two common
//! browser-side patterns. Apps wire one in at boot:
//!
//! ```ignore
//! auth_plugin()
//!     .with_token_storage(pocopine_auth_client::storage::LocalStorage::new("auth_token"))
//! ```
//!
//! ## Security
//!
//! Persisting access tokens in JavaScript-readable storage
//! (`localStorage` / `sessionStorage`) means an XSS bug can steal them.
//! For high-value applications, prefer an `httpOnly` cookie issued by
//! the server (the bearer middleware then doesn't need a token slot at
//! all — the browser sends the cookie automatically). The
//! [`LocalStorage`] / [`SessionStorage`] impls are appropriate when
//! that tradeoff is acceptable; document it for your app's threat
//! model.

use std::cell::RefCell;
use std::rc::Rc;

/// Persistence layer for the bearer token. Implementations talk to a
/// browser storage surface (or, for tests, an in-memory mock).
pub trait TokenStorage: 'static {
    /// Read the persisted token. `None` if no token has been saved or
    /// the underlying surface is unavailable.
    fn load(&self) -> Option<String>;

    /// Persist `token`, overwriting any previously-saved value.
    fn save(&self, token: &str);

    /// Drop the persisted token.
    fn clear(&self);
}

thread_local! {
    static STORAGE: RefCell<Option<Rc<dyn TokenStorage>>> = const { RefCell::new(None) };
}

pub(crate) fn install_storage(storage: Rc<dyn TokenStorage>) {
    STORAGE.with(|s| *s.borrow_mut() = Some(storage));
}

pub(crate) fn current_storage() -> Option<Rc<dyn TokenStorage>> {
    STORAGE.with(|s| s.borrow().clone())
}

#[doc(hidden)]
pub fn __reset_storage_for_test() {
    STORAGE.with(|s| *s.borrow_mut() = None);
}

// ─── In-memory default (no persistence) ─────────────────────────────

/// No-op storage. Token lives only in the in-memory slot — page reload
/// signs the user out. The default when no other storage is configured.
#[derive(Default)]
pub struct InMemory;

impl TokenStorage for InMemory {
    fn load(&self) -> Option<String> {
        None
    }
    fn save(&self, _: &str) {}
    fn clear(&self) {}
}

// ─── Browser-side localStorage / sessionStorage ─────────────────────

/// Persist the token in `window.localStorage` under a configurable
/// key. Survives page reload, browser restart, and is shared across
/// tabs of the same origin (which the cross-tab broadcast feature
/// relies on).
#[cfg(target_arch = "wasm32")]
pub struct LocalStorage {
    key: String,
}

#[cfg(target_arch = "wasm32")]
impl LocalStorage {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

#[cfg(target_arch = "wasm32")]
impl TokenStorage for LocalStorage {
    fn load(&self) -> Option<String> {
        let storage = web_sys::window()?.local_storage().ok()??;
        storage.get_item(&self.key).ok()?
    }

    fn save(&self, token: &str) {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.set_item(&self.key, token);
            }
        }
    }

    fn clear(&self) {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.remove_item(&self.key);
            }
        }
    }
}

/// Persist the token in `window.sessionStorage`. Survives navigation
/// within a tab but is dropped when the tab closes; not shared across
/// tabs. Use when "stay signed in across page reloads in this tab"
/// is the right durability tradeoff.
#[cfg(target_arch = "wasm32")]
pub struct SessionStorage {
    key: String,
}

#[cfg(target_arch = "wasm32")]
impl SessionStorage {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

#[cfg(target_arch = "wasm32")]
impl TokenStorage for SessionStorage {
    fn load(&self) -> Option<String> {
        let storage = web_sys::window()?.session_storage().ok()??;
        storage.get_item(&self.key).ok()?
    }

    fn save(&self, token: &str) {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.session_storage() {
                let _ = storage.set_item(&self.key, token);
            }
        }
    }

    fn clear(&self) {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.session_storage() {
                let _ = storage.remove_item(&self.key);
            }
        }
    }
}

/// In-memory storage with observable state for host tests. Lives at
/// the module top-level (not inside `mod tests`) so other test modules
/// in the crate (`lib.rs`) can use it for write-through assertions.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestStorage {
    pub(crate) saved: std::cell::RefCell<Option<String>>,
}

#[cfg(test)]
impl TokenStorage for TestStorage {
    fn load(&self) -> Option<String> {
        self.saved.borrow().clone()
    }
    fn save(&self, token: &str) {
        *self.saved.borrow_mut() = Some(token.to_string());
    }
    fn clear(&self) {
        *self.saved.borrow_mut() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_storage_is_a_no_op() {
        let storage = InMemory;
        storage.save("hello");
        assert_eq!(storage.load(), None);
        storage.clear();
        assert_eq!(storage.load(), None);
    }

    #[test]
    fn install_and_read_back_through_thread_local() {
        __reset_storage_for_test();
        let test = Rc::new(TestStorage::default());
        install_storage(test.clone());
        let storage = current_storage().expect("installed storage missing");
        storage.save("xyz");
        assert_eq!(test.saved.borrow().as_deref(), Some("xyz"));
        assert_eq!(storage.load().as_deref(), Some("xyz"));
        storage.clear();
        assert_eq!(test.saved.borrow().as_deref(), None);
        __reset_storage_for_test();
    }
}
