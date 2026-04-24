//! Runtime storage for component-level `#[computed]` fields.
//!
//! Each component scope installs a set of lazily-evaluated
//! `Computed<JsValue>` nodes keyed by public field name. Reads
//! through `ComponentState::get` can then resolve computed fields
//! without storing synthetic values on the author-facing struct.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsValue;

use crate::computed::Computed;
use crate::reactive::ScopeId;

struct Runtime {
    entries: HashMap<&'static str, Rc<Computed<JsValue>>>,
}

thread_local! {
    static BY_SCOPE: RefCell<HashMap<ScopeId, Rc<Runtime>>> = RefCell::new(HashMap::new());
    static BY_PTR: RefCell<HashMap<usize, Rc<Runtime>>> = RefCell::new(HashMap::new());
}

pub fn state_ptr<T>(state: &T) -> usize {
    state as *const T as usize
}

pub fn install(
    scope_id: ScopeId,
    state_ptr: usize,
    entries: Vec<(&'static str, Rc<Computed<JsValue>>)>,
) {
    let runtime = Rc::new(Runtime {
        entries: entries.into_iter().collect(),
    });
    BY_SCOPE.with(|by_scope| {
        by_scope.borrow_mut().insert(scope_id, runtime.clone());
    });
    BY_PTR.with(|by_ptr| {
        by_ptr.borrow_mut().insert(state_ptr, runtime);
    });
}

pub fn get(scope_id: ScopeId, key: &str) -> Option<JsValue> {
    BY_SCOPE.with(|by_scope| {
        by_scope
            .borrow()
            .get(&scope_id)
            .and_then(|runtime| runtime.entries.get(key))
            .map(|computed| computed.get())
    })
}

pub fn get_for_state_ptr(state_ptr: usize, key: &str) -> Option<JsValue> {
    BY_PTR.with(|by_ptr| {
        by_ptr
            .borrow()
            .get(&state_ptr)
            .and_then(|runtime| runtime.entries.get(key))
            .map(|computed| computed.get())
    })
}

pub fn clear_scope(scope_id: ScopeId) {
    let runtime = BY_SCOPE.with(|by_scope| by_scope.borrow_mut().remove(&scope_id));
    if let Some(runtime) = runtime {
        BY_PTR.with(|by_ptr| {
            by_ptr
                .borrow_mut()
                .retain(|_, candidate| !Rc::ptr_eq(candidate, &runtime));
        });
    }
}
