//! Shared runtime for `<pp-component :is="...">` and router outlets.
//!
//! The template compiler already emits custom-element mount sites and
//! child-host binding effects. `pp-component` reuses that ABI: the mount site
//! installs a region on the sentinel host, while its compiled `:is` / prop
//! bindings call [`set_binding`]. Router outlets call [`render`] directly with
//! the matched component name and route params. Both paths therefore share
//! component lookup, prop seeding, lifecycle teardown, keep-alive caching, and
//! transition handling.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{JSON, Reflect};
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::mount;
use crate::reactive::{ScopeId, effect};

const REGION_ID_KEY: &str = "__pp_dynamic_region_id";

thread_local! {
    static NEXT_REGION_ID: Cell<u64> = const { Cell::new(1) };
    static REGIONS: RefCell<HashMap<u64, Rc<RefCell<Region>>>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone)]
struct MountedComponent {
    name: &'static str,
    element: Element,
    scope_id: Option<ScopeId>,
}

struct Region {
    id: u64,
    host: Element,
    current: Option<MountedComponent>,
    cache: HashMap<&'static str, MountedComponent>,
    leaving: HashMap<u64, MountedComponent>,
    next_leave_id: u64,
    props: HashMap<String, JsValue>,
    keep_alive: bool,
}

/// Snapshot of the component currently rendered by a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MountedInfo {
    pub(crate) name: &'static str,
    pub(crate) scope_id: Option<ScopeId>,
}

/// Install the template-authored `<pp-component>` sentinel.
pub(crate) fn install(host: &Element) {
    let region = ensure_region(host);
    let initial_name = host
        .get_attribute("is")
        .filter(|name| !name.trim().is_empty());
    {
        let mut region = region.borrow_mut();
        region.keep_alive = host.has_attribute("keep-alive");
    }
    if let Some(name) = initial_name {
        set_component(&region, Some(&name));
    }
}

/// Install one compiled binding on a `<pp-component>` host.
///
/// Non-`is` bindings are installed before `:is` by the template-plan helper,
/// so the first child mount receives every authored prop during setup.
pub(crate) fn install_binding(
    host: &Element,
    parent_proxy: &JsValue,
    arg: &'static str,
    evaluator: Rc<dyn Fn(&JsValue) -> JsValue>,
) {
    let owner = host.clone();
    let host = host.clone();
    let proxy = parent_proxy.clone();
    let id = effect(move || {
        let value = evaluator(&proxy);
        set_binding(&host, arg, value);
    });
    mount::track_effect_on(&owner, id);
}

/// Update one reactive `pp-component` binding while preserving its JsValue
/// shape (objects/arrays are not stringified through a DOM attribute).
pub(crate) fn set_binding(host: &Element, arg: &str, value: JsValue) {
    let region = ensure_region(host);
    match arg {
        "is" => {
            let name = component_name_from_value(&value);
            set_component(&region, name.as_deref());
        }
        "keep-alive" => {
            region.borrow_mut().keep_alive = binding_truthy(&value);
        }
        _ => {
            let key = normalize_prop_name(arg);
            let current = {
                let mut region = region.borrow_mut();
                region.props.insert(key.clone(), value.clone());
                region.current.clone()
            };
            if let Some(current) = current {
                write_prop(&current, &key, &value);
            }
        }
    }
}

/// Render a registered component into `host`, replacing the region's dynamic
/// prop set. Used by the router for `<pp-outlet>`.
pub(crate) fn render(
    host: &Element,
    component_name: Option<&str>,
    props: &HashMap<String, JsValue>,
) -> Option<MountedInfo> {
    let region = ensure_region(host);
    {
        let mut region = region.borrow_mut();
        region.keep_alive = host.has_attribute("keep-alive");
        region.props = props.clone();
    }
    set_component(&region, component_name);
    current_info_for(&region)
}

/// Clear the active component while respecting the host's keep-alive policy.
pub(crate) fn clear(host: &Element) {
    let Some(region) = region_for(host) else {
        return;
    };
    set_component(&region, None);
}

/// Tear down every active, cached, and leaving child synchronously. Router
/// rejection/error paths use this so protected DOM cannot remain visible for
/// a leave transition.
pub(crate) fn clear_immediate(host: &Element) {
    let Some(region) = region_for(host) else {
        return;
    };
    let removed = {
        let mut region = region.borrow_mut();
        let mut removed = Vec::new();
        if let Some(current) = region.current.take() {
            removed.push(current);
        }
        removed.extend(region.cache.drain().map(|(_, mounted)| mounted));
        removed.extend(region.leaving.drain().map(|(_, mounted)| mounted));
        removed
    };
    for mounted in removed {
        mount::release_compiled_subtree(&mounted.element);
        mounted.element.remove();
    }
    // The router's built-in error surface is deliberately raw HTML rather
    // than a registered component. The sentinel owns its whole subtree, so an
    // immediate clear must remove that unmanaged fallback as well.
    host.set_text_content(None);
}

fn current_info_for(region: &Rc<RefCell<Region>>) -> Option<MountedInfo> {
    region.borrow().current.as_ref().map(|mounted| MountedInfo {
        name: mounted.name,
        scope_id: mounted.scope_id,
    })
}

/// Drop the Rust-side region state when its owning host subtree releases.
/// Child scopes are released by `mount::release_subtree`'s normal recursion.
pub(crate) fn release_host(host: &Element) {
    let Some(id) = region_id(host) else {
        return;
    };
    REGIONS.with(|regions| {
        regions.borrow_mut().remove(&id);
    });
    let _ = Reflect::delete_property(host.as_ref(), &JsValue::from_str(REGION_ID_KEY));
}

fn ensure_region(host: &Element) -> Rc<RefCell<Region>> {
    if let Some(region) = region_for(host) {
        return region;
    }
    let id = NEXT_REGION_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1).max(1));
        id
    });
    let region = Rc::new(RefCell::new(Region {
        id,
        host: host.clone(),
        current: None,
        cache: HashMap::new(),
        leaving: HashMap::new(),
        next_leave_id: 1,
        props: HashMap::new(),
        keep_alive: host.has_attribute("keep-alive"),
    }));
    REGIONS.with(|regions| {
        regions.borrow_mut().insert(id, region.clone());
    });
    let _ = Reflect::set(
        host.as_ref(),
        &JsValue::from_str(REGION_ID_KEY),
        &JsValue::from_f64(id as f64),
    );
    region
}

fn region_for(host: &Element) -> Option<Rc<RefCell<Region>>> {
    let id = region_id(host)?;
    REGIONS.with(|regions| regions.borrow().get(&id).cloned())
}

fn region_id(host: &Element) -> Option<u64> {
    Reflect::get(host.as_ref(), &JsValue::from_str(REGION_ID_KEY))
        .ok()
        .and_then(|value| value.as_f64())
        .map(|id| id as u64)
}

fn set_component(region: &Rc<RefCell<Region>>, requested: Option<&str>) {
    let canonical = requested
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(crate::registry::canonical_component_name);

    // A route error fallback can leave raw HTML in an otherwise empty region.
    // Remove it before either mounting a component or resolving to "render
    // nothing". Managed current/cached/leaving nodes are never touched here.
    {
        let state = region.borrow();
        if state.current.is_none() && state.cache.is_empty() && state.leaving.is_empty() {
            state.host.set_text_content(None);
        }
    }

    let (outgoing, incoming) = {
        let mut state = region.borrow_mut();
        if state.current.as_ref().map(|mounted| mounted.name) == canonical {
            if let Some(current) = state.current.as_ref() {
                apply_props(current, &state.props);
            }
            return;
        }

        let outgoing = state.current.take().map(|mounted| {
            if state.keep_alive {
                let name = mounted.name;
                let element = mounted.element.clone();
                state.cache.insert(name, mounted);
                Outgoing::Cached { name, element }
            } else {
                let leave_id = state.next_leave_id;
                state.next_leave_id = state.next_leave_id.wrapping_add(1).max(1);
                let element = mounted.element.clone();
                state.leaving.insert(leave_id, mounted);
                Outgoing::Remove { leave_id, element }
            }
        });

        let incoming = canonical.and_then(|name| {
            let mounted = match state.cache.remove(name) {
                Some(cached) => cached,
                None => mount_new(&state.host, name, &state.props)?,
            };
            apply_props(&mounted, &state.props);
            let element = mounted.element.clone();
            state.current = Some(mounted);
            Some(element)
        });
        (outgoing, incoming)
    };

    if let Some(outgoing) = outgoing {
        start_leave(region, outgoing);
    }
    if let Some(incoming) = incoming {
        let _ = incoming.remove_attribute("hidden");
        let _ = incoming.remove_attribute("aria-hidden");
        crate::directives::transition::enter(&incoming, || {});
    }
}

enum Outgoing {
    Cached {
        name: &'static str,
        element: Element,
    },
    Remove {
        leave_id: u64,
        element: Element,
    },
}

fn start_leave(region: &Rc<RefCell<Region>>, outgoing: Outgoing) {
    match outgoing {
        Outgoing::Cached { name, element } => {
            let region_id = region.borrow().id;
            let element_for_done = element.clone();
            crate::directives::transition::leave(&element, move || {
                let Some(region) =
                    REGIONS.with(|regions| regions.borrow().get(&region_id).cloned())
                else {
                    return;
                };
                let should_hide = {
                    let state = region.borrow();
                    state.current.as_ref().map(|current| current.name) != Some(name)
                        && state.cache.contains_key(name)
                };
                if should_hide {
                    let _ = element_for_done.set_attribute("hidden", "");
                    let _ = element_for_done.set_attribute("aria-hidden", "true");
                }
            });
        }
        Outgoing::Remove { leave_id, element } => {
            let region_id = region.borrow().id;
            crate::directives::transition::leave(&element, move || {
                finish_remove(region_id, leave_id);
            });
        }
    }
}

fn finish_remove(region_id: u64, leave_id: u64) {
    let region = REGIONS.with(|regions| regions.borrow().get(&region_id).cloned());
    let Some(region) = region else {
        return;
    };
    let removed = region.borrow_mut().leaving.remove(&leave_id);
    if let Some(removed) = removed {
        mount::release_compiled_subtree(&removed.element);
        removed.element.remove();
    }
}

fn mount_new(
    host: &Element,
    name: &'static str,
    props: &HashMap<String, JsValue>,
) -> Option<MountedComponent> {
    let doc = host.owner_document()?;
    let element = doc.create_element(name).ok()?;
    copy_forwarded_attributes(host, &element);
    for (key, value) in props {
        set_initial_prop_attribute(&element, key, value);
    }
    host.append_child(element.as_ref()).ok()?;
    mount::mount_child_component(&element, name);
    mount::finalize_compiled_subtree(&element);
    Some(MountedComponent {
        name,
        scope_id: mount::host_child_scope_id_of(&element),
        element,
    })
}

fn copy_forwarded_attributes(host: &Element, child: &Element) {
    let attrs = host.attributes();
    for index in 0..attrs.length() {
        let Some(attr) = attrs.item(index) else {
            continue;
        };
        let name = attr.name();
        if matches!(name.as_str(), "is" | "keep-alive")
            || name.starts_with("__pp_")
            || (name.starts_with("pp-") && !name.starts_with("pp-transition"))
        {
            continue;
        }
        let _ = child.set_attribute(&name, &attr.value());
    }
}

fn set_initial_prop_attribute(child: &Element, key: &str, value: &JsValue) {
    if value.is_null() || value.is_undefined() {
        let _ = child.remove_attribute(key);
        return;
    }
    let serialized = if let Some(value) = value.as_string() {
        value
    } else if let Some(value) = value.as_f64() {
        value.to_string()
    } else if let Some(value) = value.as_bool() {
        value.to_string()
    } else {
        JSON::stringify(value)
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    };
    let _ = child.set_attribute(key, &serialized);
}

fn apply_props(mounted: &MountedComponent, props: &HashMap<String, JsValue>) {
    for (key, value) in props {
        write_prop(mounted, key, value);
    }
}

fn write_prop(mounted: &MountedComponent, key: &str, value: &JsValue) {
    let Some(scope_id) = mounted.scope_id else {
        return;
    };
    let target_key =
        crate::model_runtime::resolve_model_key(scope_id, key).unwrap_or_else(|| key.to_string());
    let is_prop = crate::scope::Scope::find(scope_id)
        .map(|scope| scope.state.borrow().is_prop(&target_key))
        .unwrap_or(false);
    if !is_prop {
        return;
    }
    crate::model_runtime::with_write_origin(
        crate::model_runtime::WriteOrigin::ParentModelIn,
        || {
            let _ = crate::scope::write_field(scope_id, &target_key, value);
        },
    );
}

fn normalize_prop_name(name: &str) -> String {
    name.replace('-', "_")
}

fn component_name_from_value(value: &JsValue) -> Option<String> {
    if let Some(name) = value.as_string() {
        return Some(name);
    }
    for key in ["__pocopine_component", "name", "component", "tag"] {
        if let Ok(name) = Reflect::get(value, &JsValue::from_str(key))
            && let Some(name) = name.as_string()
        {
            return Some(name);
        }
    }
    None
}

fn binding_truthy(value: &JsValue) -> bool {
    !(value.is_null() || value.is_undefined() || value == &JsValue::FALSE)
}
