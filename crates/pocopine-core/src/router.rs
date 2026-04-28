//! Client-side SPA router.
//!
//! Shape of the runtime:
//!
//! * User-declared routes live in a `Vec<Route>` behind a thread-local
//!   (see [`register_route`]). Patterns are `/foo/:id` style, with
//!   `*` as the 404-fallback.
//! * A single `<pp-outlet>` in the DOM is the mount point
//!   ([`set_outlet`]). The mount recognises the tag and hands its
//!   element to the router.
//! * Navigation goes through [`navigate`]. It pushes a new history
//!   entry, re-matches, unmounts the prior page, and creates a
//!   `<component-name>` tag with path-params as HTML attributes inside
//!   the outlet. The existing [`crate::mount::walk`] pipeline picks
//!   it up and handles tag resolution + prop coercion.
//! * A synthetic `RouteState` scope drives the `$route` magic. The
//!   router calls [`trigger_scope`] on its id so any template binding
//!   reading `$route.path` / `$route.params.<name>` / `$route.query.<name>`
//!   re-evaluates when the URL changes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect};
use once_cell::unsync::OnceCell;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{Element, Event};

use crate::mount;
use crate::reactive::trigger_scope;
use crate::scope::{ComponentState, Scope};

// ─── route parsing ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Segment {
    Literal(String),
    Param(String),
    Wildcard,
}

#[derive(Clone, Debug)]
pub struct Route {
    pub pattern: String,
    segments: Vec<Segment>,
    pub component_name: &'static str,
}

impl Route {
    fn parse(pattern: &str, component_name: &'static str) -> Self {
        let segments = if pattern == "*" {
            vec![Segment::Wildcard]
        } else {
            pattern
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    if let Some(name) = s.strip_prefix(':') {
                        Segment::Param(name.to_string())
                    } else {
                        Segment::Literal(s.to_string())
                    }
                })
                .collect()
        };
        Route {
            pattern: pattern.to_string(),
            segments,
            component_name,
        }
    }

    fn is_wildcard(&self) -> bool {
        matches!(self.segments.as_slice(), [Segment::Wildcard])
    }

    /// Try to match `path` against this route. Returns captured params
    /// on a successful match, `None` otherwise.
    fn match_path(&self, path: &str) -> Option<HashMap<String, String>> {
        if self.is_wildcard() {
            return Some(HashMap::new());
        }
        let input: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if input.len() != self.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (seg, got) in self.segments.iter().zip(input.iter()) {
            match seg {
                Segment::Literal(s) if s == got => {}
                Segment::Literal(_) => return None,
                Segment::Param(name) => {
                    params.insert(name.clone(), (*got).to_string());
                }
                Segment::Wildcard => {}
            }
        }
        Some(params)
    }
}

// ─── synthetic `$route` scope ───────────────────────────────────────

#[derive(Default)]
struct RouteState {
    path: String,
    params: HashMap<String, String>,
    query: HashMap<String, String>,
}

impl ComponentState for RouteState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "path" => JsValue::from_str(&self.path),
            "params" => map_to_object(&self.params),
            "query" => map_to_object(&self.query),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, _key: &str, _value: JsValue) {
        // $route is read-only from templates.
    }

    fn keys(&self) -> &'static [&'static str] {
        &["path", "params", "query"]
    }

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

fn map_to_object(map: &HashMap<String, String>) -> JsValue {
    let obj = Object::new();
    for (k, v) in map {
        let _ = Reflect::set(&obj, &JsValue::from_str(k), &JsValue::from_str(v));
    }
    obj.into()
}

// ─── thread-local state ─────────────────────────────────────────────

thread_local! {
    static ROUTES: RefCell<Vec<Route>> = const { RefCell::new(Vec::new()) };
    static OUTLET: RefCell<Option<Element>> = const { RefCell::new(None) };
    static ROUTE_SCOPE: OnceCell<Scope> = const { OnceCell::new() };
    static ROUTE_STATE_RC: OnceCell<Rc<RefCell<RouteState>>> =
        const { OnceCell::new() };
    static INITIALISED: Cell<bool> = const { Cell::new(false) };
}

/// Register a route. Called from `App::route::<C>(pattern)`.
pub fn register_route(pattern: String, component_name: &'static str) {
    ROUTES.with(|r| {
        let route = Route::parse(&pattern, component_name);
        r.borrow_mut().push(route);
    });
}

/// Tell the router where to mount pages. Called from the mount when
/// it sees `<pp-outlet>`.
pub fn set_outlet(el: Element) {
    OUTLET.with(|o| *o.borrow_mut() = Some(el));
}

/// Navigate to `url`. Pushes a history entry and paints the matched
/// page.
pub fn navigate(url: &str) {
    let Some(win) = web_sys::window() else { return };
    if let Ok(history) = win.history() {
        let _ = history.push_state_with_url(&JsValue::NULL, "", Some(url));
    }
    mount_current();
}

/// Initialise the router: attach a `popstate` listener and paint the
/// current URL. Called once from `App::run` after the initial mount
/// pass.
pub fn init() {
    if INITIALISED.with(|b| b.replace(true)) {
        return; // idempotent
    }

    ensure_route_scope();

    // popstate → re-mount.
    let cb = Closure::wrap(Box::new(move |_: Event| {
        mount_current();
    }) as Box<dyn FnMut(Event)>);
    if let Some(win) = web_sys::window() {
        let _ = win.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    }
    cb.forget();

    mount_current();
}

fn ensure_route_scope() {
    ROUTE_SCOPE.with(|cell| {
        if cell.get().is_some() {
            return;
        }
        let state = Rc::new(RefCell::new(RouteState::default()));
        let scope = Scope::new(state.clone());
        let _ = cell.set(scope);
        ROUTE_STATE_RC.with(|s| {
            let _ = s.set(state);
        });
    });
}

/// Read-only proxy onto the `$route` scope. Magic resolver uses this.
pub fn route_proxy() -> JsValue {
    ensure_route_scope();
    ROUTE_SCOPE.with(|cell| {
        cell.get()
            .map(|s| s.into_proxy())
            .unwrap_or(JsValue::UNDEFINED)
    })
}

fn mount_current() {
    ensure_route_scope();

    let Some(win) = web_sys::window() else { return };
    let loc = win.location();
    let path = loc.pathname().unwrap_or_else(|_| "/".into());
    let search = loc.search().unwrap_or_default();

    // Match.
    let (component_name, params) = match_route(&path);

    // Update the route state and trigger subscribers (bindings reading
    // `$route.*` re-run).
    let query = parse_query(&search);
    ROUTE_STATE_RC.with(|cell| {
        if let Some(s) = cell.get() {
            let mut st = s.borrow_mut();
            st.path = path.clone();
            st.params = params.clone();
            st.query = query;
        }
    });
    ROUTE_SCOPE.with(|cell| {
        if let Some(scope) = cell.get() {
            trigger_scope(scope.id);
        }
    });

    // Devtools hook — fires on every resolved route change, even
    // when there's no matching component (404). The router panel
    // uses this to build its recent-history view.
    #[cfg(feature = "devtools")]
    crate::devtools::hooks::fire_route_change(&path, &params);

    let Some(name) = component_name else { return };

    // Paint into the outlet. `replace_children` removes the previous
    // page's subtree, which the MutationObserver turns into effect +
    // scope cleanup via `mount::release_subtree`.
    let outlet = OUTLET.with(|o| o.borrow().clone());
    let Some(outlet) = outlet else { return };
    let Some(doc) = win.document() else { return };

    // Build `<name key="value" ...>`; the mount handles the mount.
    let el = match doc.create_element(name) {
        Ok(e) => e,
        Err(_) => return,
    };
    for (k, v) in &params {
        let _ = el.set_attribute(k, v);
    }
    outlet.replace_children_with_node_1(el.as_ref());
    // RFC-058 Phase 6.5 — drive the route component's mount through
    // the compiled-only entry. The mount's recursive directive
    // scan is gone; route components must be `#[component]` types
    // so their template plan installs every binding/listener via
    // the macro-emitted entries.
    mount::mount_child_component(&el, name);
    mount::finalize_compiled_subtree(&el);
}

/// Find the first matching route's component name + params.
fn match_route(path: &str) -> (Option<&'static str>, HashMap<String, String>) {
    ROUTES.with(|r| {
        let routes = r.borrow();
        // Specific routes first; wildcards as a fallback.
        for route in routes.iter().filter(|r| !r.is_wildcard()) {
            if let Some(params) = route.match_path(path) {
                return (Some(route.component_name), params);
            }
        }
        for route in routes.iter().filter(|r| r.is_wildcard()) {
            if let Some(params) = route.match_path(path) {
                return (Some(route.component_name), params);
            }
        }
        (None, HashMap::new())
    })
}

fn parse_query(search: &str) -> HashMap<String, String> {
    let stripped = search.strip_prefix('?').unwrap_or(search);
    let mut out = HashMap::new();
    if stripped.is_empty() {
        return out;
    }
    for pair in stripped.split('&') {
        let mut it = pair.splitn(2, '=');
        let Some(key) = it.next() else { continue };
        if key.is_empty() {
            continue;
        }
        let value = it.next().unwrap_or("");
        out.insert(url_decode(key), url_decode(value));
    }
    out
}

fn url_decode(s: &str) -> String {
    // Minimal percent-decode + `+` → ` `. js_sys has URLSearchParams
    // but avoiding the extra feature for the host-compat path.
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex_nib(bytes[i + 1]), hex_nib(bytes[i + 2])) {
                out.push((a << 4) | b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nib(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_match() {
        let r = Route::parse("/about", "about");
        assert!(r.match_path("/about").is_some());
        assert!(r.match_path("/").is_none());
        assert!(r.match_path("/about/extra").is_none());
    }

    #[test]
    fn param_capture() {
        let r = Route::parse("/blog/:id", "blog");
        let caps = r.match_path("/blog/42").unwrap();
        assert_eq!(caps.get("id"), Some(&"42".to_string()));
    }

    #[test]
    fn mixed_segments() {
        let r = Route::parse("/users/:uid/posts/:pid", "post");
        let caps = r.match_path("/users/7/posts/99").unwrap();
        assert_eq!(caps.get("uid"), Some(&"7".to_string()));
        assert_eq!(caps.get("pid"), Some(&"99".to_string()));
    }

    #[test]
    fn wildcard_matches_anything() {
        let r = Route::parse("*", "not-found");
        assert!(r.match_path("/").is_some());
        assert!(r.match_path("/nope/anywhere").is_some());
    }

    #[test]
    fn root_path() {
        let r = Route::parse("/", "home");
        assert!(r.match_path("/").is_some());
        assert!(r.match_path("/about").is_none());
    }

    #[test]
    fn query_parsing() {
        let q = parse_query("?name=Ada&hello=world%20%26%20mars");
        assert_eq!(q.get("name"), Some(&"Ada".to_string()));
        assert_eq!(q.get("hello"), Some(&"world & mars".to_string()));
    }
}
