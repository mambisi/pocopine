//! Element-owned finalization before DOM removal or scope release (RFC-124).

use std::cell::RefCell;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, Node};

struct Finalizer {
    owner: Element,
    callback: Box<dyn FnOnce()>,
}

thread_local! {
    static FINALIZERS: RefCell<Vec<Finalizer>> = const { RefCell::new(Vec::new()) };
}

/// Invoke `callback` once before Pocopine detaches `owner` or an owning ancestor.
///
/// All finalizers in the departing subtree run before any of its scopes are
/// released. Leave transitions finalize on completion, not on start/cancellation.
/// The callback is synchronous and cannot veto removal. Capture DOM references
/// at registration and do not start new mounts or mutate the departing subtree.
/// External DOM removal cannot provide the attached-element guarantee; subsequent
/// explicit cleanup still invokes the callback, allowing `is_connected` checks.
pub fn on_before_detach(owner: &Element, callback: impl FnOnce() + 'static) {
    FINALIZERS.with(|hooks| {
        hooks.borrow_mut().push(Finalizer {
            owner: owner.clone(),
            callback: Box::new(callback),
        });
    });
}

/// Include logical ownership across teleport roots as well as DOM ancestry.
fn belongs_to(root: &Node, owner: &Element) -> bool {
    let mut current = Some(owner.clone());
    let mut seen = Vec::<Element>::new();
    while let Some(element) = current {
        if root.contains(Some(element.as_ref())) {
            return true;
        }
        if seen.iter().any(|old| old.is_same_node(Some(&element))) {
            return false;
        }
        seen.push(element.clone());
        let origin = js_sys::Reflect::get(
            element.as_ref(),
            &JsValue::from_str(crate::directives::teleport::TELEPORT_ORIGIN_KEY),
        )
        .ok()
        .and_then(|value| value.dyn_into::<Element>().ok());
        current = origin.or_else(|| element.parent_element());
    }
    false
}

/// The caller must invoke this before the first detach or scope cleanup.
/// Empty registration tables cost one thread-local lookup, with no DOM walk.
pub(crate) fn prepare(root: &Node) {
    let callbacks = FINALIZERS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        let mut ready = Vec::new();
        let mut index = 0;
        while index < hooks.len() {
            if belongs_to(root, &hooks[index].owner) {
                ready.push(hooks.remove(index).callback);
            } else {
                index += 1;
            }
        }
        ready
    });
    // Remove every selected hook before callbacks run: recursive cleanup cannot
    // invoke one twice, and callbacks never run under the registry borrow.
    for callback in callbacks {
        callback();
    }
}
