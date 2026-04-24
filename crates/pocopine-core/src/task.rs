//! Scoped async task helpers.
//!
//! `spawn_scoped` and `spawn_latest` tie task lifetime to the current
//! component scope. Cancellation in v1 is cooperative: cancelling a task
//! flips a flag and drops it from the scope registry, but a future that
//! never checks that flag may still run to completion.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use wasm_bindgen_futures::spawn_local;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

#[derive(Default)]
struct ScopeTasks {
    tasks: Vec<Rc<TaskState>>,
    latest: HashMap<String, Rc<TaskState>>,
}

struct TaskState {
    cancelled: Cell<bool>,
}

#[derive(Clone)]
pub struct TaskHandle {
    inner: Rc<TaskState>,
}

thread_local! {
    static TASKS: RefCell<HashMap<ScopeId, ScopeTasks>> = RefCell::new(HashMap::new());
}

impl TaskHandle {
    fn new() -> Self {
        Self {
            inner: Rc::new(TaskState {
                cancelled: Cell::new(false),
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancelled.set(true);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.get()
    }
}

pub fn spawn(fut: impl Future<Output = ()> + 'static) {
    spawn_local(fut);
}

pub fn spawn_scoped(fut: impl Future<Output = ()> + 'static) -> TaskHandle {
    let scope_id = current_scope_id()
        .expect("pocopine::spawn_scoped called outside a handler / lifecycle context");
    let handle = TaskHandle::new();
    TASKS.with(|tasks| {
        tasks
            .borrow_mut()
            .entry(scope_id)
            .or_default()
            .tasks
            .push(handle.inner.clone());
    });
    let inner = handle.inner.clone();
    spawn_local(async move {
        fut.await;
        let _ = inner;
    });
    handle
}

pub fn spawn_latest(
    task_name: impl Into<Cow<'static, str>>,
    fut: impl Future<Output = ()> + 'static,
) -> TaskHandle {
    let scope_id = current_scope_id()
        .expect("pocopine::spawn_latest called outside a handler / lifecycle context");
    let task_name = task_name.into().into_owned();
    let handle = TaskHandle::new();
    TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let scope_tasks = tasks.entry(scope_id).or_default();
        if let Some(prev) = scope_tasks.latest.insert(task_name.clone(), handle.inner.clone()) {
            prev.cancelled.set(true);
        }
        scope_tasks.tasks.push(handle.inner.clone());
    });
    let inner = handle.inner.clone();
    spawn_local(async move {
        fut.await;
        TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            let Some(scope_tasks) = tasks.get_mut(&scope_id) else {
                return;
            };
            if scope_tasks
                .latest
                .get(&task_name)
                .is_some_and(|current| Rc::ptr_eq(current, &inner))
            {
                scope_tasks.latest.remove(&task_name);
            }
        });
    });
    handle
}

pub fn clear_scope(scope_id: ScopeId) {
    TASKS.with(|tasks| {
        if let Some(scope_tasks) = tasks.borrow_mut().remove(&scope_id) {
            for task in scope_tasks.tasks {
                task.cancelled.set(true);
            }
        }
    });
}
