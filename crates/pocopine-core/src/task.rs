//! Scoped async task helpers.
//!
//! `spawn_scoped` and `spawn_latest` tie task lifetime to the current
//! component scope. Cancellation drops the wrapped future at its next poll;
//! the task state stores its current waker so cancellation also wakes a
//! pending future instead of waiting for the future's own I/O to complete.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

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
    waker: RefCell<Option<Waker>>,
}

impl TaskState {
    fn cancel(&self) {
        self.cancelled.set(true);
        // End the RefCell borrow before `wake`: a custom executor may poll
        // synchronously, and the poll path updates this same waker slot.
        let waker = self.waker.borrow_mut().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Cancellable<F> {
    future: Pin<Box<F>>,
    state: Rc<TaskState>,
}

impl<F: Future<Output = ()>> Cancellable<F> {
    fn new(future: F, state: Rc<TaskState>) -> Self {
        Self {
            future: Box::pin(future),
            state,
        }
    }
}

impl<F: Future<Output = ()>> Future for Cancellable<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.state.cancelled.get() {
            return Poll::Ready(());
        }
        *self.state.waker.borrow_mut() = Some(cx.waker().clone());
        match self.future.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.state.waker.borrow_mut().take();
                Poll::Ready(())
            }
            Poll::Pending if self.state.cancelled.get() => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
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
                waker: RefCell::new(None),
            }),
        }
    }

    fn cancelled() -> Self {
        let handle = Self::new();
        handle.cancel();
        handle
    }

    pub fn cancel(&self) {
        self.inner.cancel();
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
    spawn_for_scope(scope_id, fut)
}

/// Spawn a task tied to an explicit scope. If that scope has already
/// unmounted, the future is dropped and the returned handle starts in
/// the cancelled state.
pub fn spawn_for_scope(scope_id: ScopeId, fut: impl Future<Output = ()> + 'static) -> TaskHandle {
    if crate::scope::Scope::find(scope_id).is_none() {
        return TaskHandle::cancelled();
    }

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
        Cancellable::new(fut, inner.clone()).await;
        finish_task(scope_id, &inner, None);
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
    spawn_latest_for_scope(scope_id, task_name, fut)
}

/// Spawn a latest-wins task tied to an explicit scope. Reusing
/// `task_name` cancels the previous live task in that slot. If the
/// scope has already unmounted, the future is dropped and the returned
/// handle starts in the cancelled state.
pub fn spawn_latest_for_scope(
    scope_id: ScopeId,
    task_name: impl Into<Cow<'static, str>>,
    fut: impl Future<Output = ()> + 'static,
) -> TaskHandle {
    if crate::scope::Scope::find(scope_id).is_none() {
        return TaskHandle::cancelled();
    }

    let task_name = task_name.into().into_owned();
    let handle = TaskHandle::new();
    let previous = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let scope_tasks = tasks.entry(scope_id).or_default();
        let previous = scope_tasks
            .latest
            .insert(task_name.clone(), handle.inner.clone());
        scope_tasks.tasks.push(handle.inner.clone());
        previous
    });
    // Wake the superseded task only after the TASKS RefMut is gone. An
    // executor may poll synchronously from `wake`, and completion re-enters
    // the same registry through `finish_task`.
    if let Some(previous) = previous {
        previous.cancel();
    }
    let inner = handle.inner.clone();
    spawn_local(async move {
        Cancellable::new(fut, inner.clone()).await;
        finish_task(scope_id, &inner, Some(&task_name));
    });
    handle
}

fn finish_task(scope_id: ScopeId, state: &Rc<TaskState>, latest_name: Option<&str>) {
    TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let remove_scope = {
            let Some(scope_tasks) = tasks.get_mut(&scope_id) else {
                return;
            };
            scope_tasks.tasks.retain(|task| !Rc::ptr_eq(task, state));
            if let Some(name) = latest_name
                && scope_tasks
                    .latest
                    .get(name)
                    .is_some_and(|current| Rc::ptr_eq(current, state))
            {
                scope_tasks.latest.remove(name);
            }
            scope_tasks.tasks.is_empty() && scope_tasks.latest.is_empty()
        };
        if remove_scope {
            tasks.remove(&scope_id);
        }
    });
}

pub fn clear_scope(scope_id: ScopeId) {
    // Remove the registry entry under the RefCell borrow, then wake tasks
    // only after that borrow has ended. A custom executor is allowed to poll
    // synchronously from `wake`, and completion re-enters `TASKS`.
    let scope_tasks = TASKS.with(|tasks| tasks.borrow_mut().remove(&scope_id));
    if let Some(scope_tasks) = scope_tasks {
        for task in scope_tasks.tasks {
            task.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_for_scope_returns_cancelled_handle_when_scope_is_gone() {
        let handle = spawn_for_scope(ScopeId(u64::MAX), async move {});

        assert!(handle.is_cancelled());
    }

    #[test]
    fn spawn_latest_for_scope_returns_cancelled_handle_when_scope_is_gone() {
        let handle = spawn_latest_for_scope(ScopeId(u64::MAX), "search", async move {});

        assert!(handle.is_cancelled());
    }
}
