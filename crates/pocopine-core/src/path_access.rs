//! RFC-112 — typed nested-path access.
//!
//! The substrate for native deep writes and nested-signal
//! fingerprinting. Templates address state by dotted string paths
//! (`settings.theme`); the type that owns a path segment is only
//! known one level at a time, so access composes serde-style: a
//! `#[derive(PathAccess)]` on a struct handles ITS OWN fields and
//! recurses into children through the autoref dispatch below —
//! children that also derive keep descending, children that don't
//! terminate the native path and the caller falls back to the
//! RFC-024 §7 snapshot round-trip.
//!
//! Design points:
//!
//! * **Values are pushed down, not references pulled up.** A
//!   `get_mut`-shaped API can't cross `dyn ComponentState` — the
//!   leaf type varies per runtime path. Instead the `JsValue`
//!   travels down the recursion and deserializes at the leaf arm,
//!   where the concrete type is known.
//! * **Empty path = the value itself.** `path_set(&[], v)`
//!   replaces `self` (serde), which is what a `Vec` element hit
//!   with an exhausted path needs.
//! * **`Vec` traverses numeric segments; `Option` traverses
//!   through `Some`.** A `None` on the way reports `false` — the
//!   caller's fallback (and the runtime's dropped-write warning)
//!   own that case.

use wasm_bindgen::JsValue;

/// Typed nested access, implemented by `#[derive(PathAccess)]`
/// (in `pocopine-macros`) and the container impls below.
pub trait PathAccess {
    /// Write `value` at `path` relative to `self`. Empty path
    /// replaces `self`. Returns `false` when the path doesn't
    /// resolve or the value doesn't deserialize — callers fall
    /// back to the snapshot round-trip.
    fn path_set(&mut self, path: &[&str], value: &JsValue) -> bool;

    /// RFC-113 — fingerprint of the value at `path` (the nested
    /// analogue of `ComponentState::field_fingerprint`). Empty
    /// path fingerprints `self`. `None` = unreachable path.
    fn path_fingerprint(&self, path: &[&str]) -> Option<u64>;
}

impl<T: PathAccess + serde::Serialize + serde::de::DeserializeOwned> PathAccess for Vec<T> {
    fn path_set(&mut self, path: &[&str], value: &JsValue) -> bool {
        match path {
            [] => set_leaf(self, value),
            [idx, rest @ ..] => {
                let Ok(i) = idx.parse::<usize>() else {
                    return false;
                };
                match self.get_mut(i) {
                    Some(el) => el.path_set(rest, value),
                    None => false,
                }
            }
        }
    }

    fn path_fingerprint(&self, path: &[&str]) -> Option<u64> {
        match path {
            [] => crate::fingerprint::fingerprint(self),
            [idx, rest @ ..] => self.get(idx.parse::<usize>().ok()?)?.path_fingerprint(rest),
        }
    }
}

impl<T: PathAccess + serde::Serialize + serde::de::DeserializeOwned> PathAccess for Option<T> {
    fn path_set(&mut self, path: &[&str], value: &JsValue) -> bool {
        match path {
            [] => set_leaf(self, value),
            _ => match self {
                Some(inner) => inner.path_set(path, value),
                None => false,
            },
        }
    }

    fn path_fingerprint(&self, path: &[&str]) -> Option<u64> {
        match path {
            [] => crate::fingerprint::fingerprint(self),
            _ => self.as_ref()?.path_fingerprint(path),
        }
    }
}

/// Deserialize `value` into `slot`. Mirrors the macro-generated
/// `ComponentState::set` semantics, including the empty-string →
/// `null` retry (inputs emit `""` where `Option` fields expect
/// absence).
pub fn set_leaf<T: serde::de::DeserializeOwned>(slot: &mut T, value: &JsValue) -> bool {
    if let Ok(v) = serde_wasm_bindgen::from_value(value.clone()) {
        *slot = v;
        return true;
    }
    if value.as_string().as_deref() == Some("")
        && let Ok(v) = serde_wasm_bindgen::from_value(JsValue::NULL)
    {
        *slot = v;
        return true;
    }
    false
}

// ─── autoref dispatch (macro-generated call sites) ─────────────────
//
// The `#[component]` / `#[store]` / `#[derive(PathAccess)]` macros
// emit descent calls without knowing whether the child type
// implements `PathAccess`. Method resolution decides at compile
// time: the by-value impl (bound on `PathAccess`) wins when the
// bound holds; otherwise autoref finds the by-reference fallback,
// which terminates the native path (`false` / `None`) so the
// caller degrades to the snapshot round-trip.

pub struct PathSetDispatch<'a, T: ?Sized>(pub &'a mut T);

pub trait PathSetViaAccess {
    fn __poc_dispatch_set(self, path: &[&str], value: &JsValue) -> bool;
}
impl<T: PathAccess + ?Sized> PathSetViaAccess for PathSetDispatch<'_, T> {
    fn __poc_dispatch_set(self, path: &[&str], value: &JsValue) -> bool {
        self.0.path_set(path, value)
    }
}
pub trait PathSetNoAccess {
    fn __poc_dispatch_set(&self, _path: &[&str], _value: &JsValue) -> bool {
        false
    }
}
impl<T: ?Sized> PathSetNoAccess for PathSetDispatch<'_, T> {}

pub struct PathFpDispatch<'a, T: ?Sized>(pub &'a T);

pub trait PathFpViaAccess {
    fn __poc_dispatch_fp(self, path: &[&str]) -> Option<u64>;
}
impl<T: PathAccess + ?Sized> PathFpViaAccess for PathFpDispatch<'_, T> {
    fn __poc_dispatch_fp(self, path: &[&str]) -> Option<u64> {
        self.0.path_fingerprint(path)
    }
}
pub trait PathFpNoAccess {
    fn __poc_dispatch_fp(&self, _path: &[&str]) -> Option<u64> {
        None
    }
}
impl<T: ?Sized> PathFpNoAccess for PathFpDispatch<'_, T> {}
