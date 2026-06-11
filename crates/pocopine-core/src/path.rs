//! Dotted-path resolution for directive values.
//!
//! Turns `"$store.preferences.theme"` into a chain of `Reflect::get`
//! calls. Each intermediate read fires the corresponding proxy's `get`
//! trap — which calls [`crate::reactive::track`] — so dep tracking
//! walks the whole path naturally. Terminal reads of plain fields do
//! the same.

use js_sys::Reflect;
use wasm_bindgen::JsValue;

/// Walk `path` (dot-separated) starting from `root`, `Reflect::get`-ing
/// each segment. Missing / malformed segments resolve to
/// `JsValue::UNDEFINED`.
pub fn resolve_path(root: &JsValue, path: &str) -> JsValue {
    path.split('.').fold(root.clone(), |acc, segment| {
        if segment.is_empty() {
            return acc;
        }
        Reflect::get(&acc, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED)
    })
}

/// RFC-095 W1 — [`resolve_path`] with an optional
/// [`crate::expr::RootAccess`]: the FIRST segment resolves
/// Rust-side (track + field cache + `ComponentState::get`) when
/// the reader owns it; `$`-roots and reader-less calls fall back
/// to the proxy `Reflect::get`. Later segments always walk the
/// resolved (plain) value.
pub fn resolve_path_with(
    root: &JsValue,
    reader: Option<&crate::expr::RootAccess>,
    path: &str,
) -> JsValue {
    let mut segments = path.split('.').filter(|s| !s.is_empty());
    let Some(first) = segments.next() else {
        return root.clone();
    };
    // RFC-096 S2 — magic-rooted paths ride the backing scope's
    // reader (see expr::resolve_segments_with).
    if first.starts_with('$') {
        let second = segments.next();
        if let Some((access, consumed)) = crate::scope::magic_scope_access(first, second) {
            let field = if consumed == 1 {
                second
            } else {
                segments.next()
            };
            if let Some(field) = field {
                let mut cur = access.read(field).unwrap_or(JsValue::UNDEFINED);
                for seg in segments {
                    cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
                }
                return cur;
            }
            return JsValue::UNDEFINED;
        }
        // Unknown magic root — rebuild the walk from the proxy.
        let mut cur = Reflect::get(root, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED);
        if let Some(second) = second {
            cur = Reflect::get(&cur, &JsValue::from_str(second)).unwrap_or(JsValue::UNDEFINED);
        }
        for seg in segments {
            cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
        }
        return cur;
    }
    let mut cur = match reader.and_then(|a| a.read(first)) {
        Some(v) => v,
        None => Reflect::get(root, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
    };
    for segment in segments {
        cur = Reflect::get(&cur, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

/// RFC-096 S1 — [`write_path`] with an optional
/// [`crate::expr::RootAccess`]. Single-segment paths route
/// through the scoped writer (full reactivity — the same body
/// the set trap delegates to); dotted paths read the root via
/// the access (the same cached object the proxy would return)
/// and set the leaf in place, preserving the RFC-024 §7
/// surface-the-write-yourself semantics. `$`-roots and
/// access-less calls fall back to the proxy.
pub fn write_path_with(
    root: &JsValue,
    access: Option<&crate::expr::RootAccess>,
    path: &str,
    value: &JsValue,
) -> bool {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    // RFC-096 S2 — magic-rooted writes go through the backing
    // scope's writer (e.g. `pp-model="$store.user.name"`).
    if let Some(first) = segments.first() {
        if first.starts_with('$') {
            if let Some((macc, consumed)) =
                crate::scope::magic_scope_access(first, segments.get(1).copied())
            {
                match &segments[consumed..] {
                    [] => return false,
                    [field] => return macc.write(field, value),
                    [field, middle @ .., last] => {
                        let mut cur = macc.read(field).unwrap_or(JsValue::UNDEFINED);
                        for seg in middle {
                            cur = Reflect::get(&cur, &JsValue::from_str(seg))
                                .unwrap_or(JsValue::UNDEFINED);
                            if !cur.is_object() {
                                return false;
                            }
                        }
                        if !cur.is_object() {
                            return false;
                        }
                        return Reflect::set(&cur, &JsValue::from_str(last), value)
                            .unwrap_or(false);
                    }
                }
            }
        }
    }
    match segments.as_slice() {
        [] => false,
        [single] => {
            if let Some(a) = access {
                if a.write(single, value) {
                    return true;
                }
            }
            Reflect::set(root, &JsValue::from_str(single), value).unwrap_or(false)
        }
        [first, middle @ .., last] => {
            let mut target = match access.and_then(|a| a.read(first)) {
                Some(v) => v,
                None => Reflect::get(root, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
            };
            if !target.is_object() {
                return false;
            }
            for seg in middle {
                target =
                    Reflect::get(&target, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
                if !target.is_object() {
                    return false;
                }
            }
            Reflect::set(&target, &JsValue::from_str(last), value).unwrap_or(false)
        }
    }
}
