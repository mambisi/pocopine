//! Pluggable bearer-token and optimistic session-snapshot persistence.
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
//!
//! Session snapshots use the same browser surfaces, but they persist
//! only a serialized `Principal` for fast UI continuity. They are not
//! authorization proof; provider/server confirmation still owns the
//! real session state.

use std::cell::RefCell;
use std::rc::Rc;

use crate::session::AuthSessionSnapshot;

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

/// Persistence layer for an optimistic client-side identity snapshot.
///
/// Snapshots are for quick UI restore only. They must not be treated
/// as server authorization proof.
pub trait SessionSnapshotStorage: 'static {
    /// Read the persisted snapshot, if one exists and can be decoded.
    fn load_snapshot(&self) -> Option<AuthSessionSnapshot>;

    /// Persist an authenticated identity snapshot.
    fn save_snapshot(&self, snapshot: &AuthSessionSnapshot);

    /// Drop the persisted identity snapshot.
    fn clear_snapshot(&self);
}

thread_local! {
    static STORAGE: RefCell<Option<Rc<dyn TokenStorage>>> = const { RefCell::new(None) };
    static SESSION_SNAPSHOT_STORAGE: RefCell<Option<Rc<dyn SessionSnapshotStorage>>> =
        const { RefCell::new(None) };
}

pub(crate) fn install_storage(storage: Rc<dyn TokenStorage>) {
    STORAGE.with(|s| *s.borrow_mut() = Some(storage));
}

pub(crate) fn current_storage() -> Option<Rc<dyn TokenStorage>> {
    STORAGE.with(|s| s.borrow().clone())
}

pub(crate) fn install_session_snapshot_storage(storage: Rc<dyn SessionSnapshotStorage>) {
    SESSION_SNAPSHOT_STORAGE.with(|s| *s.borrow_mut() = Some(storage));
}

pub(crate) fn current_session_snapshot_storage() -> Option<Rc<dyn SessionSnapshotStorage>> {
    SESSION_SNAPSHOT_STORAGE.with(|s| s.borrow().clone())
}

#[doc(hidden)]
pub fn __reset_storage_for_test() {
    STORAGE.with(|s| *s.borrow_mut() = None);
    SESSION_SNAPSHOT_STORAGE.with(|s| *s.borrow_mut() = None);
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

impl SessionSnapshotStorage for InMemory {
    fn load_snapshot(&self) -> Option<AuthSessionSnapshot> {
        None
    }
    fn save_snapshot(&self, _: &AuthSessionSnapshot) {}
    fn clear_snapshot(&self) {}
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
        load_string(BrowserStorageKind::Local, &self.key)
    }

    fn save(&self, token: &str) {
        save_string(BrowserStorageKind::Local, &self.key, token);
    }

    fn clear(&self) {
        clear_string(BrowserStorageKind::Local, &self.key);
    }
}

#[cfg(target_arch = "wasm32")]
impl SessionSnapshotStorage for LocalStorage {
    fn load_snapshot(&self) -> Option<AuthSessionSnapshot> {
        load_json(BrowserStorageKind::Local, &self.key)
    }

    fn save_snapshot(&self, snapshot: &AuthSessionSnapshot) {
        save_json(BrowserStorageKind::Local, &self.key, snapshot);
    }

    fn clear_snapshot(&self) {
        clear_string(BrowserStorageKind::Local, &self.key);
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
        load_string(BrowserStorageKind::Session, &self.key)
    }

    fn save(&self, token: &str) {
        save_string(BrowserStorageKind::Session, &self.key, token);
    }

    fn clear(&self) {
        clear_string(BrowserStorageKind::Session, &self.key);
    }
}

#[cfg(target_arch = "wasm32")]
impl SessionSnapshotStorage for SessionStorage {
    fn load_snapshot(&self) -> Option<AuthSessionSnapshot> {
        load_json(BrowserStorageKind::Session, &self.key)
    }

    fn save_snapshot(&self, snapshot: &AuthSessionSnapshot) {
        save_json(BrowserStorageKind::Session, &self.key, snapshot);
    }

    fn clear_snapshot(&self) {
        clear_string(BrowserStorageKind::Session, &self.key);
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
enum BrowserStorageKind {
    Local,
    Session,
}

#[cfg(target_arch = "wasm32")]
fn browser_storage(kind: BrowserStorageKind) -> Option<web_sys::Storage> {
    let window = web_sys::window()?;
    match kind {
        BrowserStorageKind::Local => window.local_storage().ok()?,
        BrowserStorageKind::Session => window.session_storage().ok()?,
    }
}

#[cfg(target_arch = "wasm32")]
fn load_string(kind: BrowserStorageKind, key: &str) -> Option<String> {
    browser_storage(kind)?.get_item(key).ok()?
}

#[cfg(target_arch = "wasm32")]
fn save_string(kind: BrowserStorageKind, key: &str, value: &str) {
    if let Some(storage) = browser_storage(kind)
        && let Err(err) = storage.set_item(key, value)
    {
        tracing::warn!(
            target: "pocopine.log",
            key,
            error = ?err,
            "failed to write pocopine auth storage"
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn clear_string(kind: BrowserStorageKind, key: &str) {
    if let Some(storage) = browser_storage(kind)
        && let Err(err) = storage.remove_item(key)
    {
        tracing::warn!(
            target: "pocopine.log",
            key,
            error = ?err,
            "failed to clear pocopine auth storage"
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn load_json<T: serde::de::DeserializeOwned>(kind: BrowserStorageKind, key: &str) -> Option<T> {
    serde_json::from_str(&load_string(kind, key)?).ok()
}

#[cfg(target_arch = "wasm32")]
fn save_json<T: serde::Serialize>(kind: BrowserStorageKind, key: &str, value: &T) {
    match serde_json::to_string(value) {
        Ok(raw) => save_string(kind, key, &raw),
        Err(err) => tracing::warn!(
            target: "pocopine.log",
            key,
            error = %err,
            "failed to encode pocopine auth snapshot"
        ),
    }
}

/// In-memory storage with observable state for host tests. Lives at
/// the module top-level (not inside `mod tests`) so other test modules
/// in the crate (`lib.rs`) can use it for write-through assertions.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestStorage {
    pub(crate) saved: std::cell::RefCell<Option<String>>,
    pub(crate) snapshot: std::cell::RefCell<Option<AuthSessionSnapshot>>,
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
impl SessionSnapshotStorage for TestStorage {
    fn load_snapshot(&self) -> Option<AuthSessionSnapshot> {
        self.snapshot.borrow().clone()
    }
    fn save_snapshot(&self, snapshot: &AuthSessionSnapshot) {
        *self.snapshot.borrow_mut() = Some(snapshot.clone());
    }
    fn clear_snapshot(&self) {
        *self.snapshot.borrow_mut() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_storage_is_a_no_op() {
        let _guard = crate::test_util::lock_serial();
        let storage = InMemory;
        storage.save("hello");
        assert_eq!(storage.load(), None);
        storage.clear();
        assert_eq!(storage.load(), None);
    }

    #[test]
    fn install_and_read_back_through_thread_local() {
        let _guard = crate::test_util::lock_serial();
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

    #[test]
    fn install_and_read_snapshot_through_thread_local() {
        let _guard = crate::test_util::lock_serial();
        __reset_storage_for_test();
        let test = Rc::new(TestStorage::default());
        install_session_snapshot_storage(test.clone());
        let storage = current_session_snapshot_storage().expect("installed snapshot missing");
        let principal = pocopine_auth::Principal::from_user(pocopine_auth::AuthUser::new("u1"));
        let snapshot = AuthSessionSnapshot::new(principal).unwrap();
        storage.save_snapshot(&snapshot);
        assert_eq!(test.snapshot.borrow().as_ref(), Some(&snapshot));
        assert_eq!(storage.load_snapshot(), Some(snapshot));
        storage.clear_snapshot();
        assert_eq!(test.snapshot.borrow().as_ref(), None);
        __reset_storage_for_test();
    }
}
