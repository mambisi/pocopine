//! Instance lifetimes for statically selected component tags.
//!
//! A compiled template holds the authored host as an inert prototype. A
//! comment anchors its live replacement, so slots, refs, events and models
//! keep their ordinary component host without an extra element wrapper.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, HtmlTemplateElement, Node};

use crate::directives::for_plan::StaticChildMount;
use crate::directives::{if_::BranchEval, transition};
use crate::dynamic_component::ComponentKey;
use crate::mount;
use crate::reactive::{ScopeId, effect, without_tracking};

#[derive(Default)]
struct Instances {
    current: Option<(ComponentKey, Instance)>,
    leaving: Vec<Instance>,
}

#[derive(Clone)]
struct Instance {
    element: Element,
    released: Rc<Cell<bool>>,
}

impl Instance {
    fn new(element: Element) -> Self {
        let released = Rc::new(Cell::new(false));
        let mark_released = released.clone();
        mount::on_before_subtree_release(&element, move || mark_released.set(true));
        Self { element, released }
    }

    fn remove(self) {
        // A normal parent walk can visit the live sibling before reaching
        // our template anchor. Its scope and plugin hooks are already done.
        if !self.released.get() {
            mount::release_compiled_subtree(&self.element);
        }
        self.element.remove();
    }
}

type PendingMount = Box<dyn FnOnce()>;
const PENDING_KEY: &str = "__pp_keyed_pending";
const START_KEY: &str = "__pp_keyed_start";
const LEAVING_END_KEY: &str = "__pp_keyed_leaving_end";

/// A row's leave state belongs to the whole range, independently of an
/// outgoing child that happens to be leaving inside a still-active row.
pub(crate) fn mark_leaving(root: &Element, leaving: bool) {
    let start = first_node(root);
    if start == *root.as_ref() {
        return;
    }
    if leaving {
        let _ = js_sys::Reflect::set(&start, &LEAVING_END_KEY.into(), root);
    } else {
        let _ = js_sys::Reflect::delete_property(&start, &LEAVING_END_KEY.into());
    }
}

pub(crate) fn leaving_end(start: &Node) -> Option<Node> {
    js_sys::Reflect::get(start, &LEAVING_END_KEY.into())
        .ok()
        .and_then(|value| value.dyn_into::<Node>().ok())
}
thread_local! {
    static NEXT_PENDING: Cell<u64> = const { Cell::new(1) };
    static PENDING: RefCell<HashMap<u64, PendingMount>> = RefCell::new(HashMap::new());
}

/// Plans may run inside detached body/slot buffers. Start the controller
/// synchronously after insertion, before mount hooks, so its anchor and
/// cleanup owner belong to the live subtree rather than a temporary wrapper.
pub(crate) fn finalize(template: &Element) {
    if let Some(pending) = take_pending(template) {
        pending();
    }
}

pub(crate) fn release_pending(template: &Element) {
    drop(take_pending(template));
}

/// Structural row owners move the entire keyed root, including its live and
/// leaving hosts, rather than moving only the scope-carrying template anchor.
pub(crate) fn first_node(root: &Element) -> Node {
    js_sys::Reflect::get(root, &START_KEY.into())
        .ok()
        .and_then(|value| value.dyn_into::<Node>().ok())
        .unwrap_or_else(|| root.clone().into())
}

pub(crate) fn for_each_root(root: &Element, mut visit: impl FnMut(&Element)) {
    if root.local_name() != "template" {
        visit(root);
        return;
    }
    let mut node = first_node(root);
    loop {
        if let Some(element) = node.dyn_ref::<Element>() {
            visit(element);
        }
        if node == *root.as_ref() {
            break;
        }
        let Some(next) = node.next_sibling() else {
            break;
        };
        node = next;
    }
}

pub(crate) fn insert_before(parent: &Node, root: &Element, before: &Node) -> Result<(), JsValue> {
    let mut node = first_node(root);
    if node == *before {
        return Ok(());
    }
    loop {
        let last = node == *root.as_ref();
        let next = node.next_sibling();
        parent.insert_before(&node, Some(before))?;
        if last {
            break;
        }
        let Some(next) = next else { break };
        node = next;
    }
    Ok(())
}

fn take_pending(template: &Element) -> Option<PendingMount> {
    let id = js_sys::Reflect::get(template, &PENDING_KEY.into())
        .ok()
        .and_then(|value| value.as_f64())?;
    let _ = js_sys::Reflect::delete_property(template, &PENDING_KEY.into());
    PENDING.with(|pending| pending.borrow_mut().remove(&(id as u64)))
}

pub(crate) fn install(
    template: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticChildMount,
    template_name: &str,
    eval: BranchEval,
) {
    let id = NEXT_PENDING.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let _ = js_sys::Reflect::set(template, &PENDING_KEY.into(), &JsValue::from_f64(id as f64));
    let template_copy = template.clone();
    let proxy = proxy.clone();
    let name = template_name.to_owned();
    PENDING.with(|pending| {
        pending.borrow_mut().insert(
            id,
            Box::new(move || {
                install_now(&template_copy, scope_id, &proxy, entry, &name, eval);
            }),
        )
    });
}

fn install_now(
    template: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticChildMount,
    template_name: &str,
    eval: BranchEval,
) {
    let Some(prototype) = template
        .dyn_ref::<HtmlTemplateElement>()
        .and_then(|template| template.content().first_element_child())
    else {
        crate::templates_plan::record_plan_failure();
        return;
    };
    let Some(parent) = template.parent_element() else {
        crate::templates_plan::record_plan_failure();
        return;
    };
    let ctx_parent = mount::inherited_ctx_parent_of(template).unwrap_or(scope_id);
    if mount::host_child_scope_id_of(&parent) == Some(scope_id) {
        mount::forward_keyed_root_attributes(template, &prototype);
    }
    // Like conditional roots, a scope-carrying template must remain reachable
    // for parent prop writes and teardown. Nested sites use only a comment.
    let (anchor, owner): (Node, Element) = if mount::scope_id_of_element(template).is_some() {
        let Some(doc) = template.owner_document() else {
            return;
        };
        let start: Node = doc.create_comment("pp:key:start").into();
        if parent.insert_before(&start, Some(template)).is_err() {
            return;
        }
        let _ = js_sys::Reflect::set(template, &START_KEY.into(), &start);
        mount::on_before_subtree_release(template, move || {
            if let Some(parent) = start.parent_node() {
                let _ = parent.remove_child(&start);
            }
        });
        (template.clone().into(), template.clone())
    } else {
        let Some(doc) = template.owner_document() else {
            return;
        };
        let anchor: Node = doc.create_comment("pp:key").into();
        if parent.replace_child(&anchor, template).is_err() {
            return;
        }
        (anchor, parent)
    };
    let instances = Rc::new(RefCell::new(Instances::default()));
    let alive = Rc::new(Cell::new(true));
    let cleanup_instances = instances.clone();
    let cleanup_alive = alive.clone();
    mount::on_before_subtree_release(&owner, move || {
        cleanup_alive.set(false);
        let (current, leaving) = {
            let mut state = cleanup_instances.borrow_mut();
            (state.current.take(), std::mem::take(&mut state.leaving))
        };
        for instance in current
            .into_iter()
            .map(|(_, instance)| instance)
            .chain(leaving)
        {
            instance.remove();
        }
    });
    let proxy = proxy.clone();
    let template_name = template_name.to_owned();
    let initialized = Cell::new(false);
    let id = effect(move || {
        if !alive.get() {
            return;
        }
        let key = ComponentKey::from_value(&eval(&proxy));
        without_tracking(|| {
            let replacement = initialized.replace(true);
            if key.is_some()
                && instances.borrow().current.as_ref().map(|(key, _)| key) == key.as_ref()
            {
                return;
            }
            let outgoing = instances.borrow_mut().current.take();
            if let Some((_, instance)) = outgoing {
                let element = instance.element.clone();
                instances.borrow_mut().leaving.push(instance);
                let leaving = instances.clone();
                let element_for_done = element.clone();
                transition::leave_subtree(&element, move || {
                    let removed = {
                        let mut state = leaving.borrow_mut();
                        state
                            .leaving
                            .iter()
                            .position(|instance| instance.element == element_for_done)
                            .map(|index| state.leaving.remove(index))
                    };
                    if let Some(instance) = removed {
                        instance.remove();
                    }
                });
            }
            let Some(key) = key else {
                web_sys::console::error_1(&JsValue::from_str(
                    "pocopine: component `pp-key` requires a string, finite number, boolean, or null; derive a stable scalar key for composite identities",
                ));
                return;
            };
            let Some(element) = prototype
                .clone_node_with_deep(true)
                .ok()
                .and_then(|node| node.dyn_into::<Element>().ok())
            else {
                return;
            };
            mount::bind_borrowed_scope_to(&element, scope_id, &proxy);
            let _ = js_sys::Reflect::set(
                element.as_ref(),
                &mount::CTX_PARENT_KEY.into(),
                &JsValue::from_f64(ctx_parent.0 as f64),
            );
            let Some(parent) = anchor.parent_node() else {
                return;
            };
            if parent.insert_before(&element, Some(&anchor)).is_err() {
                return;
            }
            crate::templates_plan::mount_static_child_instance(
                &element,
                scope_id,
                &proxy,
                entry,
                &template_name,
            );
            instances.borrow_mut().current = Some((key, Instance::new(element.clone())));
            mount::finalize_compiled_subtree(&element);
            // Initial enter belongs to an enclosing structural controller
            // (if any). Only identity replacements start an enter here.
            if replacement {
                transition::enter_subtree(&element, || {});
            }
        });
    });
    mount::track_effect_on(&owner, id);
}
