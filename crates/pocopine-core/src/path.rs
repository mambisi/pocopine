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
                // RFC-113 — leaf-granular subscription when the
                // field's type has PathAccess reach; root-tracked
                // walk otherwise.
                let rest: Vec<&str> = segments.collect();
                if !rest.is_empty() {
                    let mut full = Vec::with_capacity(rest.len() + 1);
                    full.push(field);
                    full.extend_from_slice(&rest);
                    if let Some(v) = access.read_path(&full) {
                        return v;
                    }
                }
                let mut cur = access.read(field).unwrap_or(JsValue::UNDEFINED);
                for seg in rest {
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
    let rest: Vec<&str> = segments.collect();
    if !rest.is_empty()
        && let Some(a) = reader
    {
        let mut full = Vec::with_capacity(rest.len() + 1);
        full.push(first);
        full.extend_from_slice(&rest);
        // RFC-113 — leaf-granular subscription (see the magic
        // branch above).
        if let Some(v) = a.read_path(&full) {
            return v;
        }
    }
    let mut cur = match reader.and_then(|a| a.read(first)) {
        Some(v) => v,
        None => Reflect::get(root, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
    };
    for segment in rest {
        cur = Reflect::get(&cur, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

/// RFC-096 S1 — [`write_path`] with an optional
/// [`crate::expr::RootAccess`]. Single-segment paths route
/// through the scoped writer (full reactivity — the same body
/// the set trap delegates to). Dotted paths read the root field
/// via the access (a projection snapshot of Rust state), set the
/// leaf on that snapshot, then write the whole field back through
/// the scoped writer — the RFC-024 §7 "surface the write" done by
/// the framework instead of the author, so nested `pp-model`
/// paths deserialize + trigger like flat ones. `$`-roots that
/// don't resolve to a scoped writer (`$event` & co) are live JS
/// objects: the in-place leaf set IS the write.
pub fn write_path_with(
    root: &JsValue,
    access: Option<&crate::expr::RootAccess>,
    path: &str,
    value: &JsValue,
) -> bool {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    write_segments_with(root, access, &segments, value)
}

/// Segments-form core of [`write_path_with`], shared with the
/// assignment evaluator (`expr::write_assign_path_with`) so
/// `pp-model="a.b"` and `@click="a.b = x"` cannot diverge.
pub(crate) fn write_segments_with(
    root: &JsValue,
    access: Option<&crate::expr::RootAccess>,
    segments: &[&str],
    value: &JsValue,
) -> bool {
    // RFC-096 S2 — magic-rooted writes go through the backing
    // scope's writer (e.g. `pp-model="$store.user.name"`).
    if let Some(first) = segments.first()
        && first.starts_with('$')
        && let Some((macc, consumed)) =
            crate::scope::magic_scope_access(first, segments.get(1).copied())
    {
        return match &segments[consumed..] {
            [] => false,
            [field] => macc.write(field, value),
            [field, rest @ ..] => {
                // RFC-112 — native leaf write first: leaf-typed
                // deserialize, no container round-trip, and the
                // precise trigger lattice (sibling leaves stay
                // quiet). Falls through when the path has no
                // PathAccess reach.
                let mut full = Vec::with_capacity(rest.len() + 1);
                full.push(*field);
                full.extend_from_slice(rest);
                if macc.write_path(&full, value) {
                    return true;
                }
                let container = macc.read(field).unwrap_or(JsValue::UNDEFINED);
                if !set_leaf(&container, rest, value, segments) {
                    return false;
                }
                // The container is a projection snapshot, not Rust
                // state — surface the deep write by writing the
                // whole field back through the scoped writer:
                // deserialize + invalidate + trigger, same as a
                // flat write.
                macc.write(field, &container)
            }
        };
    }
    match segments {
        [] => false,
        [single] => {
            if let Some(a) = access
                && a.write(single, value)
            {
                return true;
            }
            Reflect::set(root, &JsValue::from_str(single), value).unwrap_or(false)
        }
        [first, rest @ ..] => {
            // RFC-112 — native leaf write first (see the magic
            // branch above). `$`-roots never resolve natively.
            if !first.starts_with('$')
                && let Some(a) = access
                && a.write_path(segments, value)
            {
                return true;
            }
            let container = match access.and_then(|a| a.read(first)) {
                Some(v) => v,
                None => Reflect::get(root, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
            };
            if !set_leaf(&container, rest, value, segments) {
                return false;
            }
            if first.starts_with('$') {
                // `$event` & co resolve to live platform objects —
                // the in-place set IS the write; there is no Rust
                // field to surface it to.
                return true;
            }
            // Surface the write (RFC-024 §7): scoped writer first,
            // proxy set trap (which delegates to the same writer)
            // as the fallback.
            if let Some(a) = access
                && a.write(first, &container)
            {
                return true;
            }
            Reflect::set(root, &JsValue::from_str(first), &container).unwrap_or(false)
        }
    }
}

/// Walk `rest[..len-1]` from `container` and set the final segment
/// on the object it lands on. A non-object anywhere on the way
/// drops the write with a console warning — a lost `pp-model`
/// keystroke is otherwise invisible.
fn set_leaf(container: &JsValue, rest: &[&str], value: &JsValue, segments: &[&str]) -> bool {
    let Some((last, middle)) = rest.split_last() else {
        return false;
    };
    // The container segment is the one right before `rest`.
    let mut blame = segments[segments.len() - rest.len() - 1];
    let mut cur = container.clone();
    for seg in middle {
        if !cur.is_object() {
            return warn_dropped(segments, blame);
        }
        cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
        blame = seg;
    }
    if !cur.is_object() {
        return warn_dropped(segments, blame);
    }
    Reflect::set(&cur, &JsValue::from_str(last), value).unwrap_or(false)
}

fn warn_dropped(segments: &[&str], blame: &str) -> bool {
    web_sys::console::warn_1(&JsValue::from_str(&format!(
        "pocopine: write to `{}` dropped — `{blame}` is not an object",
        segments.join(".")
    )));
    false
}
