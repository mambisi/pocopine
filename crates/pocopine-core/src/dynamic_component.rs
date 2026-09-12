//! Shared runtime for `<pp-component :is="...">` and router outlets.
//!
//! The template compiler already emits custom-element mount sites and
//! child-host binding effects. `pp-component` reuses that ABI: the mount site
//! installs a region on the sentinel host, while its compiled `:is` / prop
//! bindings call [`install_bindings`]. Router outlets call [`render`] directly with
//! the matched component name and route params. Both paths therefore share
//! component lookup, prop seeding, lifecycle teardown, keep-alive caching, and
//! transition handling.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Reflect;
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
    key: ComponentKey,
    element: Element,
    scope_id: Option<ScopeId>,
}

/// Value identity, deliberately distinct from a child's ordinary `:key` prop.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub(crate) enum ComponentKey {
    #[default]
    None,
    String(String),
    Number(String),
    Bool(bool),
}

impl ComponentKey {
    pub(crate) fn from_value(value: &JsValue) -> Option<Self> {
        if value.is_null() || value.is_undefined() {
            Some(Self::None)
        } else if let Some(value) = value.as_string() {
            Some(Self::String(value))
        } else if let Some(value) = value.as_bool() {
            Some(Self::Bool(value))
        } else {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|value| {
                    Self::Number(if value == 0.0 {
                        "0".into()
                    } else {
                        value.to_string()
                    })
                })
        }
    }
}

type BindingEvaluator = (&'static str, Rc<dyn Fn(&JsValue) -> JsValue>);

#[derive(Clone, Copy)]
struct ErasedComponentRef {
    host: &'static str,
    name: &'static str,
}

struct Region {
    id: u64,
    host: Element,
    expected_host: Option<String>,
    current: Option<MountedComponent>,
    key: ComponentKey,
    cache: HashMap<(&'static str, ComponentKey), MountedComponent>,
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
    {
        let mut region = region.borrow_mut();
        region.keep_alive = host.has_attribute("keep-alive");
    }
    if host
        .get_attribute("is")
        .is_some_and(|name| !name.trim().is_empty())
    {
        web_sys::console::error_1(&JsValue::from_str(
            "pocopine: `<pp-component is=\"...\">` cannot select a raw component name; bind a host-scoped `ComponentRef` through `:is`",
        ));
    }
}

/// Attach the owning component identity before reactive bindings run.
///
/// A `ComponentRef` minted for one host must not be replayed into another
/// host's outlet, even though both values share the same wire representation.
pub(crate) fn configure_host(host: &Element, expected_host: &str) {
    ensure_region(host).borrow_mut().expected_host = Some(expected_host.to_owned());
}

/// Install one effect for the region's selection, identity, and forwarded props.
/// Evaluate every expression before mounting or updating a child; otherwise a
/// key change can run setup with props from the preceding identity.
pub(crate) fn install_bindings(
    host: &Element,
    parent_proxy: &JsValue,
    bindings: Vec<BindingEvaluator>,
) {
    let owner = host.clone();
    let host = host.clone();
    let proxy = parent_proxy.clone();
    let id = effect(move || {
        let values: Vec<_> = bindings
            .iter()
            .map(|(arg, eval)| (*arg, eval(&proxy)))
            .collect();
        crate::reactive::without_tracking(|| set_bindings(&host, values));
    });
    mount::track_effect_on(&owner, id);
}

fn set_bindings(host: &Element, values: Vec<(&str, JsValue)>) {
    let region = ensure_region(host);
    let mut selection = JsValue::UNDEFINED;
    let mut key = Some(ComponentKey::None);
    let mut changed_props = HashMap::new();
    {
        let mut state = region.borrow_mut();
        for (arg, value) in values {
            match arg {
                "is" => selection = value,
                "pp-key" => key = ComponentKey::from_value(&value),
                "keep-alive" => state.keep_alive = binding_truthy(&value),
                _ => {
                    let prop = normalize_prop_name(arg);
                    if state.props.get(&prop) != Some(&value) {
                        changed_props.insert(prop.clone(), value.clone());
                        state.props.insert(prop, value);
                    }
                }
            }
        }
        if let Some(key) = &key {
            state.key = key.clone();
        }
    }
    if key.is_none() {
        web_sys::console::error_1(&JsValue::from_str(
            "pocopine: `<pp-component pp-key>` requires a string, finite number, boolean, or null; derive a stable scalar key for composite identities",
        ));
        set_component(&region, None);
        return;
    }
    match component_ref_from_value(&selection) {
        Ok(Some(selection)) => {
            let expected_host = region.borrow().expected_host.clone();
            if expected_host.as_deref() == Some(selection.host) {
                set_component_with_props(&region, Some(selection.name), Some(&changed_props));
            } else {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "pocopine: dynamic selection for host `{}` cannot be used by `<pp-component>` owned by `{}`; construct it with `ComponentRef::of::<Child>()` in a `ComponentRef<Host>` context for this host",
                    selection.host,
                    expected_host.as_deref().unwrap_or("<unknown>"),
                )));
                set_component(&region, None);
            }
        }
        Ok(None) => set_component(&region, None),
        Err(error) => {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "pocopine: `<pp-component :is>` requires `ComponentRef<Host>` or \
                 `Option<ComponentRef<Host>>`; raw component-name strings are rejected. \
                 Construct typed selections with `ComponentRef::of::<Child>()` in a typed \
                 `ComponentRef<Host>` context, or validate external names with \
                 `ComponentRef::<Host>::from_registered_name(...)`: {error}",
            )));
            set_component(&region, None);
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
        region.key = ComponentKey::None;
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
        expected_host: None,
        current: None,
        key: ComponentKey::None,
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
    set_component_with_props(region, requested, None);
}

fn set_component_with_props(
    region: &Rc<RefCell<Region>>,
    requested: Option<&str>,
    changed_props: Option<&HashMap<String, JsValue>>,
) {
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
        if state
            .current
            .as_ref()
            .map(|mounted| (mounted.name, &mounted.key))
            == canonical.map(|name| (name, &state.key))
        {
            if let Some(current) = state.current.as_ref() {
                apply_props(current, changed_props.unwrap_or(&state.props));
            }
            return;
        }

        let outgoing = state.current.take().map(|mounted| {
            if state.keep_alive {
                let identity = (mounted.name, mounted.key.clone());
                let element = mounted.element.clone();
                state.cache.insert(identity.clone(), mounted);
                Outgoing::Cached { identity, element }
            } else {
                let leave_id = state.next_leave_id;
                state.next_leave_id = state.next_leave_id.wrapping_add(1).max(1);
                let element = mounted.element.clone();
                state.leaving.insert(leave_id, mounted);
                Outgoing::Remove { leave_id, element }
            }
        });

        let incoming = canonical.and_then(|name| {
            let identity = (name, state.key.clone());
            let mounted = match state.cache.remove(&identity) {
                Some(cached) => cached,
                None => mount_new(&state.host, name, state.key.clone(), &state.props)?,
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
        identity: (&'static str, ComponentKey),
        element: Element,
    },
    Remove {
        leave_id: u64,
        element: Element,
    },
}

fn start_leave(region: &Rc<RefCell<Region>>, outgoing: Outgoing) {
    match outgoing {
        Outgoing::Cached { identity, element } => {
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
                    state
                        .current
                        .as_ref()
                        .map(|current| (current.name, &current.key))
                        != Some((identity.0, &identity.1))
                        && state.cache.contains_key(&identity)
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
    key: ComponentKey,
    props: &HashMap<String, JsValue>,
) -> Option<MountedComponent> {
    let doc = host.owner_document()?;
    let element = doc.create_element(name).ok()?;
    copy_forwarded_attributes(host, &element);
    host.append_child(element.as_ref()).ok()?;
    mount::mount_child_component_seeded(&element, name, None, props);
    mount::finalize_compiled_subtree(&element);
    Some(MountedComponent {
        name,
        key,
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

fn component_ref_from_value(value: &JsValue) -> Result<Option<ErasedComponentRef>, String> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }

    #[derive(serde::Deserialize)]
    struct Wire {
        __pocopine_host: String,
        __pocopine_component: String,
    }

    let wire =
        serde_wasm_bindgen::from_value::<Wire>(value.clone()).map_err(|error| error.to_string())?;
    let host = crate::registry::canonical_component_name(&wire.__pocopine_host)
        .ok_or_else(|| format!("dynamic host `{}` is not registered", wire.__pocopine_host))?;
    let name =
        crate::registry::canonical_component_name(&wire.__pocopine_component).ok_or_else(|| {
            format!(
                "dynamic component `{}` is not registered",
                wire.__pocopine_component,
            )
        })?;
    Ok(Some(ErasedComponentRef { host, name }))
}

fn binding_truthy(value: &JsValue) -> bool {
    !(value.is_null() || value.is_undefined() || value == &JsValue::FALSE)
}
